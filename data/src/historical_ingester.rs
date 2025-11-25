//! Service for downloading historical data from Binance Data Vision and writing to cache.
//!
//! This service is responsible for:
//! - Downloading historical K-line and Tick data from Binance Data Vision
//! - Writing downloaded data to Parquet cache files
//! - Managing cache directory structure

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::io::{Cursor, Read};
use std::collections::HashSet;
use tokio::fs::{self, File as TokioFile};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use reqwest::Client;
use arrow::array::{Array, ArrayRef, Float64Array, TimestampMicrosecondArray, UInt32Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use parquet::arrow::arrow_writer::ArrowWriter;
use zip::ZipArchive;
use csv;

use crate::{data_error::DataError, data_path, kline::KLine, compute::vp::TickDataBuffer};

/// Service for downloading and caching historical data.
#[derive(Clone)]
pub struct HistoricalIngesterService {
    client: Client,
    pub(crate) cache_dir: PathBuf,
    // Track ongoing downloads to prevent duplicate concurrent downloads
    downloading: Arc<Mutex<HashSet<String>>>,
}

impl HistoricalIngesterService {
    /// Creates a new `HistoricalIngesterService`.
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        let cache_dir = cache_dir.unwrap_or_else(|| data_path(Some("cache")));
        Self {
            client: Client::new(),
            cache_dir,
            downloading: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Downloads historical K-line data for a specific date and writes it to cache.
    pub async fn download_and_cache_kline(
        &self,
        symbol: &str,
        date: &str, // Format: "YYYY-MM-DD"
        timeframe: &str, // e.g., "1m", "5m", "1h"
    ) -> Result<Vec<KLine>, DataError> {
        // 1. Check if cache already exists
        let cache_key = format!("{}_{}_{}.parquet", symbol, date, timeframe);
        let cache_path = self.cache_dir.join(&cache_key);
        
        if cache_path.exists() {
            // Cache already exists, load from cache
            return self.load_klines_from_cache(&cache_path).await;
        }

        // 2. Check if download is already in progress (prevent duplicate downloads)
        let download_key = format!("kline:{}_{}_{}", symbol, date, timeframe);
        {
            let mut downloading = self.downloading.lock().await;
            if downloading.contains(&download_key) {
                // Another task is already downloading this file, wait for it to complete
                drop(downloading);
                for _ in 0..30 {
                    // Wait up to 3 seconds (30 * 100ms)
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    if cache_path.exists() {
                        return self.load_klines_from_cache(&cache_path).await;
                    }
                }
                // If still not available after waiting, proceed with download anyway
            } else {
                downloading.insert(download_key.clone());
            }
        }

        // 3. Download from Binance Data Vision
        let result = self.download_kline_for_date(symbol, date, timeframe).await;

        // 4. Write to cache if download succeeded
        let klines = match result {
            Ok(klines) => {
                if !klines.is_empty() {
                    if let Err(e) = self.save_klines_to_cache(&cache_path, &klines).await {
                        log::warn!("Failed to save klines to cache: {}", e);
                    }
                }
                Ok(klines)
            }
            Err(e) => Err(e),
        };

        // 5. Remove from downloading set
        {
            let mut downloading = self.downloading.lock().await;
            downloading.remove(&download_key);
        }

        klines
    }

    /// Downloads historical Tick data for a specific date and writes it to cache.
    pub async fn download_and_cache_ticks(
        &self,
        symbol: &str,
        date: &str, // Format: "YYYY-MM-DD"
    ) -> Result<TickDataBuffer, DataError> {
        // 1. Check if cache already exists
        let cache_key = format!("{}_{}_ticks.parquet", symbol, date);
        let cache_path = self.cache_dir.join(&cache_key);
        
        if cache_path.exists() {
            // Cache already exists, load from cache
            return self.load_ticks_from_cache(&cache_path).await;
        }

        // 2. Check if download is already in progress (prevent duplicate downloads)
        let download_key = format!("ticks:{}_{}", symbol, date);
        {
            let mut downloading = self.downloading.lock().await;
            if downloading.contains(&download_key) {
                // Another task is already downloading this file, wait for it to complete
                // by checking cache periodically
                drop(downloading);
                for _ in 0..30 {
                    // Wait up to 3 seconds (30 * 100ms)
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    if cache_path.exists() {
                        return self.load_ticks_from_cache(&cache_path).await;
                    }
                }
                // If still not available after waiting, proceed with download anyway
                // (the other download might have failed)
            } else {
                downloading.insert(download_key.clone());
            }
        }

        // 3. Download from Binance Data Vision
        let result = self.download_ticks_for_date(symbol, date).await;

        // 4. Write to cache if download succeeded
        let ticks = match result {
            Ok(ticks) => {
                if !ticks.prices.is_empty() {
                    if let Err(e) = self.save_ticks_to_cache(&cache_path, &ticks).await {
                        log::warn!("Failed to save ticks to cache: {}", e);
                    }
                }
                Ok(ticks)
            }
            Err(e) => Err(e),
        };

        // 5. Remove from downloading set
        {
            let mut downloading = self.downloading.lock().await;
            downloading.remove(&download_key);
        }

        ticks
    }

    /// Downloads K-line data for a specific date from Binance Data Vision.
    async fn download_kline_for_date(
        &self,
        symbol: &str,
        date: &str,
        timeframe: &str,
    ) -> Result<Vec<KLine>, DataError> {
        // Convert timeframe to Binance interval format
        let interval = match timeframe {
            "1m" => "1m",
            "3m" => "3m",
            "5m" => "5m",
            "15m" => "15m",
            "30m" => "30m",
            "1h" => "1h",
            "2h" => "2h",
            "4h" => "4h",
            "6h" => "6h",
            "8h" => "8h",
            "12h" => "12h",
            "1d" => "1d",
            "3d" => "3d",
            "1w" => "1w",
            "1M" => "1mo",
            _ => {
                log::warn!("Unsupported timeframe {}, defaulting to 1m", timeframe);
                "1m"
            }
        };

        // URL format: https://data.binance.vision/data/spot/daily/klines/{SYMBOL}/{INTERVAL}/{SYMBOL}-{INTERVAL}-{DATE}.zip
        let url = format!(
            "https://data.binance.vision/data/spot/daily/klines/{}/{}/{}-{}-{}.zip",
            symbol, interval, symbol, interval, date
        );

        // Downloading K-line data from Binance Data Vision

        // Download ZIP file
        let response = self.client.get(&url).send().await?;
        if !response.status().is_success() {
            // Handle 404 gracefully - historical data for today may not be available yet
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                log::warn!(
                    "Historical K-line data not available for {}/{}/{} (404). This is normal for today's data which may not be published yet (2-6 hour delay).",
                    symbol,
                    timeframe,
                    date
                );
                return Ok(Vec::new());
            }
            return Err(DataError::Network(
                reqwest::Error::from(response.error_for_status().unwrap_err())
            ));
        }

        let zip_bytes = response.bytes().await?;

        // Extract and parse CSV from ZIP
        let mut archive = ZipArchive::new(Cursor::new(zip_bytes))?;
        
        // Find the CSV file in the ZIP (usually the only file)
        let mut csv_content = String::new();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            if file.name().ends_with(".csv") {
                file.read_to_string(&mut csv_content)?;
                break;
            }
        }

        if csv_content.is_empty() {
            return Err(DataError::InvalidInput("No CSV file found in ZIP archive"));
        }

        // Parse CSV
        // Format: Open time, Open, High, Low, Close, Volume, Close time, Quote asset volume, Number of trades, ...
        let mut reader = csv::Reader::from_reader(csv_content.as_bytes());
        let mut klines = Vec::new();

        for result in reader.records() {
            let record = result?;
            if record.len() < 9 {
                continue; // Skip invalid records
            }

            let open_time_ms: u64 = record.get(0)
                .ok_or_else(|| DataError::InvalidInput("Missing open_time"))?
                .parse()
                .map_err(|_| DataError::InvalidInput("Invalid open_time"))?;
            
            let open: f64 = record.get(1)
                .ok_or_else(|| DataError::InvalidInput("Missing open"))?
                .parse()
                .map_err(|_| DataError::InvalidInput("Invalid open"))?;
            
            let high: f64 = record.get(2)
                .ok_or_else(|| DataError::InvalidInput("Missing high"))?
                .parse()
                .map_err(|_| DataError::InvalidInput("Invalid high"))?;
            
            let low: f64 = record.get(3)
                .ok_or_else(|| DataError::InvalidInput("Missing low"))?
                .parse()
                .map_err(|_| DataError::InvalidInput("Invalid low"))?;
            
            let close: f64 = record.get(4)
                .ok_or_else(|| DataError::InvalidInput("Missing close"))?
                .parse()
                .map_err(|_| DataError::InvalidInput("Invalid close"))?;
            
            let volume: f64 = record.get(5)
                .ok_or_else(|| DataError::InvalidInput("Missing volume"))?
                .parse()
                .map_err(|_| DataError::InvalidInput("Invalid volume"))?;
            
            let num_trades: u32 = record.get(8)
                .ok_or_else(|| DataError::InvalidInput("Missing num_trades"))?
                .parse()
                .map_err(|_| DataError::InvalidInput("Invalid num_trades"))?;

            klines.push(KLine {
                open_time_us: open_time_ms * 1_000, // Convert ms to us
                open,
                high,
                low,
                close,
                volume,
                num_trades,
            });
        }

        Ok(klines)
    }

    /// Downloads Tick data for a specific date from Binance Data Vision.
    async fn download_ticks_for_date(
        &self,
        symbol: &str,
        date: &str,
    ) -> Result<TickDataBuffer, DataError> {
        // URL format: https://data.binance.vision/data/spot/daily/aggTrades/{SYMBOL}/{SYMBOL}-aggTrades-{DATE}.zip
        let url = format!(
            "https://data.binance.vision/data/spot/daily/aggTrades/{}/{}-aggTrades-{}.zip",
            symbol, symbol, date
        );

        // Downloading Tick data from Binance Data Vision (logged only on first attempt per date)

        // Download ZIP file
        let response = self.client.get(&url).send().await?;
        if !response.status().is_success() {
            // Handle 404 gracefully - historical data for today may not be available yet
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                log::warn!(
                    "Historical Tick data not available for {}/{} (404). This is normal for today's data which may not be published yet (2-6 hour delay).",
                    symbol,
                    date
                );
                return Ok(TickDataBuffer {
                    prices: Vec::new(),
                    volumes: Vec::new(),
                    time_range: None,
                });
            }
            return Err(DataError::Network(
                reqwest::Error::from(response.error_for_status().unwrap_err())
            ));
        }

        let zip_bytes = response.bytes().await?;

        // Extract and parse CSV from ZIP
        let mut archive = ZipArchive::new(Cursor::new(zip_bytes))?;
        
        // Find the CSV file in the ZIP (usually the only file)
        let mut csv_content = String::new();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            if file.name().ends_with(".csv") {
                file.read_to_string(&mut csv_content)?;
                break;
            }
        }

        if csv_content.is_empty() {
            return Err(DataError::InvalidInput("No CSV file found in ZIP archive"));
        }

        // Parse CSV
        // Format: Agg trade ID, Price, Quantity, First trade ID, Last trade ID, Timestamp, Was the buyer the maker
        let mut reader = csv::Reader::from_reader(csv_content.as_bytes());
        let mut prices = Vec::new();
        let mut volumes = Vec::new();
        let mut min_timestamp: Option<u64> = None;
        let mut max_timestamp: Option<u64> = None;

        for result in reader.records() {
            let record = result?;
            if record.len() < 7 {
                continue; // Skip invalid records
            }

            let price_str = record.get(1)
                .ok_or_else(|| DataError::InvalidInput("Missing price"))?;
            let quantity_str = record.get(2)
                .ok_or_else(|| DataError::InvalidInput("Missing quantity"))?;
            let timestamp_str = record.get(5)
                .ok_or_else(|| DataError::InvalidInput("Missing timestamp"))?;

            let price: f64 = price_str.parse()
                .map_err(|_| DataError::InvalidInput("Invalid price"))?;
            let quantity: f64 = quantity_str.parse()
                .map_err(|_| DataError::InvalidInput("Invalid quantity"))?;
            // Timestamp is in milliseconds, convert to microseconds
            let timestamp_ms: u64 = timestamp_str.parse()
                .map_err(|_| DataError::InvalidInput("Invalid timestamp"))?;
            let timestamp_us = timestamp_ms * 1_000;

            // Convert price to fixed-point u32 (price * 100)
            // Note: This matches the TickDataBuffer format used in VP computation
            let price_fixed = (price * 100.0) as u32;
            
            prices.push(price_fixed);
            volumes.push(quantity as f32);
            
            // Track time range
            if min_timestamp.is_none() || timestamp_us < min_timestamp.unwrap() {
                min_timestamp = Some(timestamp_us);
            }
            if max_timestamp.is_none() || timestamp_us > max_timestamp.unwrap() {
                max_timestamp = Some(timestamp_us);
            }
        }

        // Calculate time range from timestamps
        let time_range = if let (Some(min), Some(max)) = (min_timestamp, max_timestamp) {
            Some((min, max))
        } else {
            None
        };
        
        Ok(TickDataBuffer { 
            prices, 
            volumes,
            time_range,
        })
    }

    /// Saves K-line data to cache.
    pub async fn save_klines_to_cache(
        &self,
        cache_path: &Path,
        klines: &[KLine],
    ) -> Result<(), DataError> {
        if klines.is_empty() {
            return Ok(());
        }

        // Create directory if it doesn't exist
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let record_batch = klines_to_record_batch(klines)?;
        let mut buffer = Vec::new();
        let mut writer = ArrowWriter::try_new(&mut buffer, record_batch.schema(), None)?;
        writer.write(&record_batch)?;
        writer.close()?;

        let mut file = TokioFile::create(cache_path).await?;
        file.write_all(&buffer).await?;
        file.flush().await?;
        Ok(())
    }

    /// Saves Tick data to cache.
    pub async fn save_ticks_to_cache(
        &self,
        cache_path: &Path,
        ticks: &TickDataBuffer,
    ) -> Result<(), DataError> {
        if ticks.prices.is_empty() {
            return Ok(());
        }

        // Create directory if it doesn't exist
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let record_batch = ticks_to_record_batch(ticks)?;
        let mut buffer = Vec::new();
        let mut writer = ArrowWriter::try_new(&mut buffer, record_batch.schema(), None)?;
        writer.write(&record_batch)?;
        writer.close()?;

        let mut file = TokioFile::create(cache_path).await?;
        file.write_all(&buffer).await?;
        file.flush().await?;
        Ok(())
    }

    /// Loads K-line data from cache.
    pub async fn load_klines_from_cache(
        &self,
        cache_path: &Path,
    ) -> Result<Vec<KLine>, DataError> {
        let mut file = TokioFile::open(cache_path).await?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).await?;

        let bytes = Bytes::from(buffer);
        let builder = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes)?;
        let mut reader = builder.with_batch_size(8192).build()?;

        let mut all_klines = Vec::new();
        if let Some(batch_result) = reader.next() {
            let batch = batch_result?;
            all_klines.extend(record_batch_to_klines(&batch)?);
        }
        Ok(all_klines)
    }

    /// Loads Tick data from cache.
    pub async fn load_ticks_from_cache(
        &self,
        cache_path: &Path,
    ) -> Result<TickDataBuffer, DataError> {
        let mut file = TokioFile::open(cache_path).await?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).await?;

        let bytes = Bytes::from(buffer);
        let builder = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes)?;
        let mut reader = builder.with_batch_size(8192).build()?;

        let mut all_ticks = TickDataBuffer {
            prices: Vec::new(),
            volumes: Vec::new(),
            time_range: None,
        };
        if let Some(batch_result) = reader.next() {
            let batch = batch_result?;
            let ticks = record_batch_to_ticks(&batch)?;
            all_ticks.prices.extend(ticks.prices);
            all_ticks.volumes.extend(ticks.volumes);
        }
        Ok(all_ticks)
    }
}

/// Converts K-lines to Arrow RecordBatch.
fn klines_to_record_batch(klines: &[KLine]) -> Result<RecordBatch, DataError> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("open_time_us", DataType::Timestamp(TimeUnit::Microsecond, None), false),
        Field::new("open", DataType::Float64, false),
        Field::new("high", DataType::Float64, false),
        Field::new("low", DataType::Float64, false),
        Field::new("close", DataType::Float64, false),
        Field::new("volume", DataType::Float64, false),
        Field::new("num_trades", DataType::UInt32, false),
    ]));

    let mut open_time_us_builder = TimestampMicrosecondArray::builder(klines.len());
    let mut open_builder = Float64Array::builder(klines.len());
    let mut high_builder = Float64Array::builder(klines.len());
    let mut low_builder = Float64Array::builder(klines.len());
    let mut close_builder = Float64Array::builder(klines.len());
    let mut volume_builder = Float64Array::builder(klines.len());
    let mut num_trades_builder = UInt32Array::builder(klines.len());

    for kline in klines {
        open_time_us_builder.append_value(kline.open_time_us as i64);
        open_builder.append_value(kline.open);
        high_builder.append_value(kline.high);
        low_builder.append_value(kline.low);
        close_builder.append_value(kline.close);
        volume_builder.append_value(kline.volume);
        num_trades_builder.append_value(kline.num_trades);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(open_time_us_builder.finish()),
        Arc::new(open_builder.finish()),
        Arc::new(high_builder.finish()),
        Arc::new(low_builder.finish()),
        Arc::new(close_builder.finish()),
        Arc::new(volume_builder.finish()),
        Arc::new(num_trades_builder.finish()),
    ];

    Ok(RecordBatch::try_new(schema, columns)?)
}

/// Converts Arrow RecordBatch to K-lines.
fn record_batch_to_klines(batch: &RecordBatch) -> Result<Vec<KLine>, DataError> {
    let open_time_us_array = batch
        .column_by_name("open_time_us")
        .ok_or(DataError::InvalidInput("Missing 'open_time_us' column"))?
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .ok_or(DataError::InvalidInput("Invalid 'open_time_us' column type"))?;
    let open_array = batch
        .column_by_name("open")
        .ok_or(DataError::InvalidInput("Missing 'open' column"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(DataError::InvalidInput("Invalid 'open' column type"))?;
    let high_array = batch
        .column_by_name("high")
        .ok_or(DataError::InvalidInput("Missing 'high' column"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(DataError::InvalidInput("Invalid 'high' column type"))?;
    let low_array = batch
        .column_by_name("low")
        .ok_or(DataError::InvalidInput("Missing 'low' column"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(DataError::InvalidInput("Invalid 'low' column type"))?;
    let close_array = batch
        .column_by_name("close")
        .ok_or(DataError::InvalidInput("Missing 'close' column"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(DataError::InvalidInput("Invalid 'close' column type"))?;
    let volume_array = batch
        .column_by_name("volume")
        .ok_or(DataError::InvalidInput("Missing 'volume' column"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(DataError::InvalidInput("Invalid 'volume' column type"))?;
    let num_trades_array = batch
        .column_by_name("num_trades")
        .ok_or(DataError::InvalidInput("Missing 'num_trades' column"))?
        .as_any()
        .downcast_ref::<UInt32Array>()
        .ok_or(DataError::InvalidInput("Invalid 'num_trades' column type"))?;

    let mut klines = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        let timestamp_value = open_time_us_array.value(i);
        // TimestampMicrosecondArray::value() returns microseconds as i64
        // But if the value is suspiciously large (looks like nanoseconds), convert it
        let open_time_us = if timestamp_value > 1_000_000_000_000_000 {
            // Value looks like nanoseconds, convert to microseconds
            (timestamp_value / 1_000) as u64
        } else {
            timestamp_value as u64
        };
        
        klines.push(KLine {
            open_time_us,
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

/// Converts TickDataBuffer to Arrow RecordBatch.
fn ticks_to_record_batch(ticks: &TickDataBuffer) -> Result<RecordBatch, DataError> {
    // Include time_range metadata in the schema (only if data is not empty)
    let mut schema_fields = vec![
        Field::new("price", DataType::UInt32, false), // TickDataBuffer uses u32 for prices
        Field::new("volume", DataType::Float32, false), // TickDataBuffer uses f32 for volumes
    ];
    
    // Add time_range metadata fields if available and data is not empty
    let has_time_range = ticks.time_range.is_some() && !ticks.prices.is_empty();
    if has_time_range {
        schema_fields.push(Field::new("time_range_start_us", DataType::UInt64, true)); // Nullable
        schema_fields.push(Field::new("time_range_end_us", DataType::UInt64, true)); // Nullable
    }
    
    let schema = Arc::new(Schema::new(schema_fields));

    use arrow::array::{UInt32Array, Float32Array, UInt64Array};
    let mut price_builder = UInt32Array::builder(ticks.prices.len());
    let mut volume_builder = Float32Array::builder(ticks.volumes.len());

    for (price, volume) in ticks.prices.iter().zip(ticks.volumes.iter()) {
        price_builder.append_value(*price);
        volume_builder.append_value(*volume);
    }

    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(price_builder.finish()),
        Arc::new(volume_builder.finish()),
    ];
    
    // Add time_range metadata if available (store only in first row, rest are null)
    if has_time_range {
        let (start, end) = ticks.time_range.unwrap();
        let num_rows = ticks.prices.len();
        let mut start_builder = UInt64Array::builder(num_rows);
        let mut end_builder = UInt64Array::builder(num_rows);
        
        // Store time_range only in first row, rest are null
        start_builder.append_value(start);
        end_builder.append_value(end);
        for _ in 1..num_rows {
            start_builder.append_null();
            end_builder.append_null();
        }
        
        columns.push(Arc::new(start_builder.finish()));
        columns.push(Arc::new(end_builder.finish()));
    }

    Ok(RecordBatch::try_new(schema, columns)?)
}

/// Converts Arrow RecordBatch to TickDataBuffer.
fn record_batch_to_ticks(batch: &RecordBatch) -> Result<TickDataBuffer, DataError> {
    use arrow::array::{UInt32Array, Float32Array, UInt64Array};
    let price_array = batch
        .column_by_name("price")
        .ok_or(DataError::InvalidInput("Missing 'price' column"))?
        .as_any()
        .downcast_ref::<UInt32Array>()
        .ok_or(DataError::InvalidInput("Invalid 'price' column type"))?;
    let volume_array = batch
        .column_by_name("volume")
        .ok_or(DataError::InvalidInput("Missing 'volume' column"))?
        .as_any()
        .downcast_ref::<Float32Array>()
        .ok_or(DataError::InvalidInput("Invalid 'volume' column type"))?;

    let mut prices = Vec::with_capacity(batch.num_rows());
    let mut volumes = Vec::with_capacity(batch.num_rows());
    
    for i in 0..batch.num_rows() {
        prices.push(price_array.value(i));
        volumes.push(volume_array.value(i));
    }
    
    // Try to restore time_range from metadata columns (if present)
    // Time range is stored in the first row, rest are null
    let time_range = if let (Some(start_col), Some(end_col)) = (
        batch.column_by_name("time_range_start_us"),
        batch.column_by_name("time_range_end_us"),
    ) {
        let start_array = start_col
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or(DataError::InvalidInput("Invalid 'time_range_start_us' column type"))?;
        let end_array = end_col
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or(DataError::InvalidInput("Invalid 'time_range_end_us' column type"))?;
        
        // Read from first row (where time_range is stored)
        if start_array.len() > 0 && end_array.len() > 0 && start_array.is_valid(0) && end_array.is_valid(0) {
            Some((start_array.value(0), end_array.value(0)))
        } else {
            None
        }
    } else {
        None
    };
    
    Ok(TickDataBuffer { 
        prices, 
        volumes,
        time_range,
    })
}

