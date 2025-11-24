//! Service for downloading historical data from Binance Data Vision and writing to cache.
//!
//! This service is responsible for:
//! - Downloading historical K-line and Tick data from Binance Data Vision
//! - Writing downloaded data to Parquet cache files
//! - Managing cache directory structure

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::io::{Cursor, Read};
use tokio::fs::{self, File as TokioFile};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use reqwest::Client;
use arrow::array::{ArrayRef, Float64Array, TimestampMicrosecondArray, UInt32Array};
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
}

impl HistoricalIngesterService {
    /// Creates a new `HistoricalIngesterService`.
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        let cache_dir = cache_dir.unwrap_or_else(|| data_path(Some("cache")));
        Self {
            client: Client::new(),
            cache_dir,
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
        let cache_path = self.cache_dir.join(cache_key);
        
        if cache_path.exists() {
            // Cache already exists, load from cache
            return self.load_klines_from_cache(&cache_path).await;
        }

        // 2. Download from Binance Data Vision
        let klines = self.download_kline_for_date(symbol, date, timeframe).await?;

        // 3. Write to cache
        if !klines.is_empty() {
            self.save_klines_to_cache(&cache_path, &klines).await?;
        }

        Ok(klines)
    }

    /// Downloads historical Tick data for a specific date and writes it to cache.
    pub async fn download_and_cache_ticks(
        &self,
        symbol: &str,
        date: &str, // Format: "YYYY-MM-DD"
    ) -> Result<TickDataBuffer, DataError> {
        // 1. Check if cache already exists
        let cache_key = format!("{}_{}_ticks.parquet", symbol, date);
        let cache_path = self.cache_dir.join(cache_key);
        
        if cache_path.exists() {
            // Cache already exists, load from cache
            return self.load_ticks_from_cache(&cache_path).await;
        }

        // 2. Download from Binance Data Vision
        let ticks = self.download_ticks_for_date(symbol, date).await?;

        // 3. Write to cache
        if !ticks.prices.is_empty() {
            self.save_ticks_to_cache(&cache_path, &ticks).await?;
        }

        Ok(ticks)
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

        log::info!("Downloading K-line data from Binance Data Vision: {}", url);

        // Download ZIP file
        let response = self.client.get(&url).send().await?;
        if !response.status().is_success() {
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

        log::info!("Downloaded {} K-lines for {}/{}/{}", klines.len(), symbol, timeframe, date);
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

        log::info!("Downloading Tick data from Binance Data Vision: {}", url);

        // Download ZIP file
        let response = self.client.get(&url).send().await?;
        if !response.status().is_success() {
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

        for result in reader.records() {
            let record = result?;
            if record.len() < 7 {
                continue; // Skip invalid records
            }

            let price_str = record.get(1)
                .ok_or_else(|| DataError::InvalidInput("Missing price"))?;
            let quantity_str = record.get(2)
                .ok_or_else(|| DataError::InvalidInput("Missing quantity"))?;

            let price: f64 = price_str.parse()
                .map_err(|_| DataError::InvalidInput("Invalid price"))?;
            let quantity: f64 = quantity_str.parse()
                .map_err(|_| DataError::InvalidInput("Invalid quantity"))?;

            // Convert price to fixed-point u32 (price * 100)
            // Note: This matches the TickDataBuffer format used in VP computation
            let price_fixed = (price * 100.0) as u32;
            
            prices.push(price_fixed);
            volumes.push(quantity as f32);
        }

        log::info!("Downloaded {} ticks for {}/{}", prices.len(), symbol, date);
        Ok(TickDataBuffer { prices, volumes })
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
        klines.push(KLine {
            open_time_us: open_time_us_array.value(i) as u64,
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
    let schema = Arc::new(Schema::new(vec![
        Field::new("price", DataType::UInt32, false), // TickDataBuffer uses u32 for prices
        Field::new("volume", DataType::Float32, false), // TickDataBuffer uses f32 for volumes
    ]));

    use arrow::array::{UInt32Array, Float32Array};
    let mut price_builder = UInt32Array::builder(ticks.prices.len());
    let mut volume_builder = Float32Array::builder(ticks.volumes.len());

    for (price, volume) in ticks.prices.iter().zip(ticks.volumes.iter()) {
        price_builder.append_value(*price);
        volume_builder.append_value(*volume);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(price_builder.finish()),
        Arc::new(volume_builder.finish()),
    ];

    Ok(RecordBatch::try_new(schema, columns)?)
}

/// Converts Arrow RecordBatch to TickDataBuffer.
fn record_batch_to_ticks(batch: &RecordBatch) -> Result<TickDataBuffer, DataError> {
    use arrow::array::{UInt32Array, Float32Array};
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
    
    Ok(TickDataBuffer { prices, volumes })
}

