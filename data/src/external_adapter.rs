use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::Deserialize;
use std::fmt;
use std::io::Cursor;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{self, File as TokioFile};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use arrow::array::{ArrayRef, Float64Array, TimestampNanosecondArray, UInt32Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::arrow_writer::ArrowWriter;

use crate::{arbiter_error::ArbiterError, data_path, io_service::TimeRange, kline::KLine};

/// A service for fetching historical data from external sources.
///
/// It holds a `reqwest` client for making network requests and is responsible
/// for caching strategies to minimize API calls.
#[derive(Clone)]
pub struct ExternalAdapter {
    client: reqwest::Client,
}

/// Represents a single K-line from the Binance API, which is a mixed-type array.
/// e.g., [1499040000000, "0.01634790", "0.80000000", "0.01575800", "0.01577100", ...]
/// This struct helps in deserializing this format.
#[derive(Deserialize, Debug)]
#[serde(transparent)]
struct BinanceKLine(#[serde(with = "binance_kline_format")] KLine);

impl ExternalAdapter {
    /// Creates a new `ExternalAdapter`.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    /// Fetches historical K-lines for a given symbol and time range.
    ///
    /// This is an asynchronous operation that first checks a local cache.
    /// If the data is not in the cache, it fetches it from the Binance API.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The trading symbol (e.g., "BTCUSDT").
    /// * `range` - The time range for which to fetch data.
    pub async fn fetch_historical_kline(
        &self,
        symbol: &str,
        range: TimeRange,
    ) -> Result<Vec<KLine>, ArbiterError> {
        let cache_path = get_cache_path(symbol, range).await?;

        // --- 1. Check local cache ---
        if cache_path.exists() {
            match load_klines_from_cache(&cache_path).await {
                Ok(klines) => {
                    log::info!("Cache hit for {}. Loaded {} klines from cache.", symbol, klines.len());
                    return Ok(klines);
                }
                Err(e) => {
                    log::warn!("Failed to load klines from cache {}: {}", cache_path.display(), e);
                }
            }
        }

        // --- 2. Fetch from network (Cache Miss) ---
        log::info!("Cache miss for {}. Fetching from network.", symbol);
        let url = format!(
            "https://api.binance.com/api/v3/klines?symbol={}&interval=1m&startTime={}&endTime={}&limit=1000",
            symbol,
            range.start_ns / 1_000_000, // Binance uses milliseconds
            range.end_ns / 1_000_000
        );

        let binance_klines: Vec<BinanceKLine> = self.client.get(&url).send().await?.json().await?;

        let klines: Vec<KLine> = binance_klines.into_iter().map(|bk| bk.0).collect();

        // --- 3. Write to cache ---
        if !klines.is_empty() {
            if let Err(e) = save_klines_to_cache(&cache_path, &klines).await {
                log::error!("Failed to save klines to cache {}: {}", cache_path.display(), e);
            } else {
                log::info!("Saved {} klines to cache for {}.", klines.len(), symbol);
            }
        }

        Ok(klines)
    }
}

impl Default for ExternalAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Generates a unique cache file path for K-lines.
async fn get_cache_path(symbol: &str, range: TimeRange) -> Result<PathBuf, ArbiterError> {
    let cache_dir = data_path(Some("klines_cache"));
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).await?;
    }
    let file_name = format!("{}_{}_{}.parquet", symbol, range.start_ns, range.end_ns);
    Ok(cache_dir.join(file_name))
}

/// Converts a slice of `KLine` objects into an Arrow `RecordBatch`.
fn klines_to_record_batch(klines: &[KLine]) -> Result<RecordBatch, ArbiterError> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("open_time_ns", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("open", DataType::Float64, false),
        Field::new("high", DataType::Float64, false),
        Field::new("low", DataType::Float64, false),
        Field::new("close", DataType::Float64, false),
        Field::new("volume", DataType::Float64, false),
        Field::new("num_trades", DataType::UInt32, false),
    ]));

    let mut open_time_ns_builder = TimestampNanosecondArray::builder(klines.len());
    let mut open_builder = Float64Array::builder(klines.len());
    let mut high_builder = Float64Array::builder(klines.len());
    let mut low_builder = Float64Array::builder(klines.len());
    let mut close_builder = Float64Array::builder(klines.len());
    let mut volume_builder = Float64Array::builder(klines.len());
    let mut num_trades_builder = UInt32Array::builder(klines.len());

    for kline in klines {
        open_time_ns_builder.append_value(kline.open_time_ns as i64);
        open_builder.append_value(kline.open);
        high_builder.append_value(kline.high);
        low_builder.append_value(kline.low);
        close_builder.append_value(kline.close);
        volume_builder.append_value(kline.volume);
        num_trades_builder.append_value(kline.num_trades);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(open_time_ns_builder.finish()),
        Arc::new(open_builder.finish()),
        Arc::new(high_builder.finish()),
        Arc::new(low_builder.finish()),
        Arc::new(close_builder.finish()),
        Arc::new(volume_builder.finish()),
        Arc::new(num_trades_builder.finish()),
    ];

    Ok(RecordBatch::try_new(schema, columns)?)
}

/// Converts an Arrow `RecordBatch` into a `Vec<KLine>`.
fn record_batch_to_klines(batch: &RecordBatch) -> Result<Vec<KLine>, ArbiterError> {
    let open_time_ns_array = batch
        .column_by_name("open_time_ns")
        .ok_or(ArbiterError::InvalidInput("Missing 'open_time_ns' column"))?
        .as_any()
        .downcast_ref::<TimestampNanosecondArray>()
        .ok_or(ArbiterError::InvalidInput("Invalid 'open_time_ns' column type"))?;
    let open_array = batch
        .column_by_name("open")
        .ok_or(ArbiterError::InvalidInput("Missing 'open' column"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(ArbiterError::InvalidInput("Invalid 'open' column type"))?;
    let high_array = batch
        .column_by_name("high")
        .ok_or(ArbiterError::InvalidInput("Missing 'high' column"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(ArbiterError::InvalidInput("Invalid 'high' column type"))?;
    let low_array = batch
        .column_by_name("low")
        .ok_or(ArbiterError::InvalidInput("Missing 'low' column"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(ArbiterError::InvalidInput("Invalid 'low' column type"))?;
    let close_array = batch
        .column_by_name("close")
        .ok_or(ArbiterError::InvalidInput("Missing 'close' column"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(ArbiterError::InvalidInput("Invalid 'close' column type"))?;
    let volume_array = batch
        .column_by_name("volume")
        .ok_or(ArbiterError::InvalidInput("Missing 'volume' column"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(ArbiterError::InvalidInput("Invalid 'volume' column type"))?;
    let num_trades_array = batch
        .column_by_name("num_trades")
        .ok_or(ArbiterError::InvalidInput("Missing 'num_trades' column"))?
        .as_any()
        .downcast_ref::<UInt32Array>()
        .ok_or(ArbiterError::InvalidInput("Invalid 'num_trades' column type"))?;

    let mut klines = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        klines.push(KLine {
            open_time_ns: open_time_ns_array.value(i) as u64,
            open: open_array.value(i),
            high: high_array.value(i),
            low: low_array.value(i),
            close: close_array.value(i),
            volume: volume_array.value(i),
            num_trades: num_trades_array.value(i),
        });
    }
    Ok(klines)
}

/// Loads K-lines from a Parquet cache file.
async fn load_klines_from_cache(path: &Path) -> Result<Vec<KLine>, ArbiterError> {
    let mut file = TokioFile::open(path).await?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await?;

    let bytes = Bytes::from(buffer);
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes)?;
    let mut reader = builder.with_batch_size(8192).build()?;

    let mut all_klines = Vec::new();
    if let Some(batch_result) = reader.next() {
        let batch = batch_result?;
        all_klines.extend(record_batch_to_klines(&batch)?);
    }
    Ok(all_klines)
}

/// Saves K-lines to a Parquet cache file.
async fn save_klines_to_cache(path: &Path, klines: &[KLine]) -> Result<(), ArbiterError> {
    if klines.is_empty() {
        return Ok(());
    }
    let record_batch = klines_to_record_batch(klines)?;
    let mut buffer = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut buffer, record_batch.schema(), None)?;
    writer.write(&record_batch)?;
    writer.close()?;

    let mut file = TokioFile::create(path).await?;
    file.write_all(&buffer).await?;
    file.flush().await?;
    Ok(())
}


/// Custom serde deserializer for Binance's mixed-type array K-line format.
mod binance_kline_format {
    use super::*;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<KLine, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct KLineVisitor(PhantomData<KLine>);

        impl<'de> Visitor<'de> for KLineVisitor {
            type Value = KLine;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a sequence of kline data")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let open_time_ns: u64 = seq.next_element()?.ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let open_str: &str = seq.next_element()?.ok_or_else(|| de::Error::invalid_length(1, &self))?;
                let high_str: &str = seq.next_element()?.ok_or_else(|| de::Error::invalid_length(2, &self))?;
                let low_str: &str = seq.next_element()?.ok_or_else(|| de::Error::invalid_length(3, &self))?;
                let close_str: &str = seq.next_element()?.ok_or_else(|| de::Error::invalid_length(4, &self))?;
                let volume_str: &str = seq.next_element()?.ok_or_else(|| de::Error::invalid_length(5, &self))?;
                
                // Binance sends more fields, we need to consume them to avoid errors,
                // but we don't need to store them all.
                let _close_time: u64 = seq.next_element()?.ok_or_else(|| de::Error::invalid_length(6, &self))?;
                let _quote_asset_volume: &str = seq.next_element()?.ok_or_else(|| de::Error::invalid_length(7, &self))?;
                let num_trades: u32 = seq.next_element()?.ok_or_else(|| de::Error::invalid_length(8, &self))?;

                // Consume remaining fields
                while seq.next_element::<serde_json::Value>()?.is_some() {}

                Ok(KLine {
                    open_time_ns: open_time_ns * 1_000_000, // Convert ms to ns
                    open: open_str.parse().map_err(de::Error::custom)?,
                    high: high_str.parse().map_err(de::Error::custom)?,
                    low: low_str.parse().map_err(de::Error::custom)?,
                    close: close_str.parse().map_err(de::Error::custom)?,
                    volume: volume_str.parse().map_err(de::Error::custom)?,
                    num_trades,
                })
            }
        }

        deserializer.deserialize_seq(KLineVisitor(PhantomData))
    }
}
