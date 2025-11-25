//! Historical data download executor for downloading and caching data.
//!
//! This executor is responsible for:
//! - Downloading historical K-line and Tick data from Binance Data Vision
//! - Writing downloaded data to Parquet cache files
//! - Reading cached data from Parquet files
//! - Managing cache directory structure
//!
//! The executor performs the actual download and cache operations, but does not
//! manage task queues, retry logic, or state. Those responsibilities belong to
//! `HistoricalDownloadCoordinator`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::collections::HashSet;
use tokio::fs::{self, File as TokioFile};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use arrow::array::{Array, ArrayRef, Float64Array, TimestampMicrosecondArray, UInt32Array};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use parquet::arrow::arrow_writer::ArrowWriter;

use exchange::{AdapterRegistry, HistoricalData, HistoricalDataType};
use crate::{data_error::DataError, data_path, kline::KLine, compute::vp::TickDataBuffer, symbol_resolver::SymbolResolver};

/// Executor for downloading and caching historical data.
///
/// This executor performs the actual download and cache operations. It is called
/// by `HistoricalDownloadCoordinator` to execute download tasks.
#[derive(Clone)]
pub struct HistoricalDownloadExecutor {
    adapter_registry: Arc<AdapterRegistry>,
    pub(crate) cache_dir: PathBuf,
    // Track ongoing downloads to prevent duplicate concurrent downloads
    downloading: Arc<Mutex<HashSet<String>>>,
    symbol_resolver: SymbolResolver,
}

impl HistoricalDownloadExecutor {
    /// Creates a new `HistoricalDownloadExecutor`.
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        let cache_dir = cache_dir.unwrap_or_else(|| data_path(Some("cache")));
        Self {
            adapter_registry: Arc::new(AdapterRegistry::new()),
            cache_dir,
            downloading: Arc::new(Mutex::new(HashSet::new())),
            symbol_resolver: SymbolResolver::new(),
        }
    }
    
    /// Gets the cache directory path.
    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    /// Downloads historical K-line data for a specific date and writes it to cache.
    pub async fn download_and_cache_kline(
        &self,
        symbol: &str,
        date: &str, // Format: "YYYY-MM-DD"
        timeframe: &str, // e.g., "1m", "5m", "1h"
    ) -> Result<Vec<KLine>, DataError> {
        let normalized_symbol = self.symbol_resolver.normalize(symbol, None);
        let cache_key = self.symbol_resolver.kline_cache_key(&normalized_symbol, date, timeframe);
        let cache_path = self.cache_dir.join(&cache_key);
        let download_key = self.symbol_resolver.download_key("kline", &normalized_symbol, date, Some(timeframe));
        
        // Try loading from cache first
        if cache_path.exists() {
            log::debug!("[DownloadExecutor] Loading from cache: {}", cache_path.display());
            return self.load_klines_from_cache(&cache_path).await;
        }

        // Wait for concurrent download or start new one
        if let Some(klines) = self.wait_for_concurrent_download_klines(&cache_path, &download_key).await {
            return Ok(klines);
        }

        // Acquire download lock
        {
            let mut downloading = self.downloading.lock().await;
            if downloading.contains(&download_key) {
                return Err(DataError::InvalidInput("Download already in progress".to_string()));
            }
            downloading.insert(download_key.clone());
        }

        // Download through exchange adapter
        let exchange = self.symbol_resolver.infer_exchange(&normalized_symbol);
        let adapter = self.adapter_registry
            .get_or_err(exchange)
            .map_err(|e| DataError::InvalidInput(format!("No adapter found: {}", e).to_string()))?;
        
        let historical_data = adapter
            .fetch_historical_data(
                &normalized_symbol,
                date,
                HistoricalDataType::Kline { timeframe: timeframe.to_string() },
            )
            .await
            .map_err(|e| DataError::Adapter(e.to_string().to_string()))?;
        
        // Convert to KLine format
        let result = match historical_data {
            HistoricalData::Klines(klines) => {
                Ok(klines.into_iter().map(KLine::from).collect::<Vec<KLine>>())
            }
            _ => Err(DataError::InvalidInput("Unexpected data type".to_string())),
        };

        // Save to cache if successful
        if let Ok(ref klines) = result {
            if !klines.is_empty() {
                if let Err(e) = self.save_klines_to_cache(&cache_path, klines).await {
                    log::warn!("Failed to save klines to cache: {}", e);
                }
            }
        }

        // Release lock
        {
            let mut downloading = self.downloading.lock().await;
            downloading.remove(&download_key);
        }

        result
    }

    /// Downloads historical Tick data for a specific date and writes it to cache.
    pub async fn download_and_cache_ticks(
        &self,
        symbol: &str,
        date: &str, // Format: "YYYY-MM-DD"
    ) -> Result<TickDataBuffer, DataError> {
        let normalized_symbol = self.symbol_resolver.normalize(symbol, None);
        let cache_key = self.symbol_resolver.tick_cache_key(&normalized_symbol, date);
        let cache_path = self.cache_dir.join(&cache_key);
        let download_key = self.symbol_resolver.download_key("ticks", &normalized_symbol, date, None);
        
        // Try loading from cache first
        if cache_path.exists() {
            return self.load_ticks_from_cache(&cache_path).await;
        }

        // Wait for concurrent download or start new one
        if let Some(ticks) = self.wait_for_concurrent_download_ticks(&cache_path, &download_key).await {
            return Ok(ticks);
        }

        // Acquire download lock
        {
            let mut downloading = self.downloading.lock().await;
            if downloading.contains(&download_key) {
                return Err(DataError::InvalidInput("Download already in progress".to_string()));
            }
            downloading.insert(download_key.clone());
        }

        // Download through exchange adapter
        let exchange = self.symbol_resolver.infer_exchange(&normalized_symbol);
        let adapter = self.adapter_registry
            .get_or_err(exchange)
            .map_err(|e| DataError::InvalidInput(format!("No adapter found: {}", e).to_string()))?;
        
        let historical_data = adapter
            .fetch_historical_data(
                &normalized_symbol,
                date,
                HistoricalDataType::Tick,
            )
            .await
            .map_err(|e| DataError::Adapter(e.to_string().to_string()))?;
        
        // Convert to TickDataBuffer format
        let result = match historical_data {
            HistoricalData::Ticks(trades) => {
                let mut prices = Vec::new();
                let mut volumes = Vec::new();
                let mut min_timestamp: Option<u64> = None;
                let mut max_timestamp: Option<u64> = None;
                
                for trade in trades {
                    // Convert price to fixed-point u32 (price * 100)
                    let price_fixed = (trade.price.to_f32() * 100.0) as u32;
                    prices.push(price_fixed);
                    volumes.push(trade.qty);
                    
                    // Track time range (convert ms to us)
                    let timestamp_us = trade.time * 1_000;
                    if min_timestamp.is_none() || timestamp_us < min_timestamp.unwrap() {
                        min_timestamp = Some(timestamp_us);
                    }
                    if max_timestamp.is_none() || timestamp_us > max_timestamp.unwrap() {
                        max_timestamp = Some(timestamp_us);
                    }
                }
                
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
            _ => Err(DataError::InvalidInput("Unexpected data type".to_string())),
        };

        // Save to cache if successful
        if let Ok(ref ticks) = result {
            if !ticks.prices.is_empty() {
                if let Err(e) = self.save_ticks_to_cache(&cache_path, ticks).await {
                    log::warn!("Failed to save ticks to cache: {}", e);
                }
            }
        }

        // Release lock
        {
            let mut downloading = self.downloading.lock().await;
            downloading.remove(&download_key);
        }

        result
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
        
        // Read all batches (not just the first one)
        let mut batch_count = 0;
        while let Some(batch_result) = reader.next() {
            let batch = batch_result?;
            let ticks = record_batch_to_ticks(&batch)?;
            batch_count += 1;
            
            // Merge time_range: use the first non-None time_range we find
            // Time range should be the same across all batches (stored in first row of first batch)
            if all_ticks.time_range.is_none() {
                all_ticks.time_range = ticks.time_range;
            }
            
            all_ticks.prices.extend(ticks.prices);
            all_ticks.volumes.extend(ticks.volumes);
        }
        
        log::debug!(
            "[DownloadExecutor] Loaded {} ticks from {} batches, cache: {} (time_range: {:?})",
            all_ticks.prices.len(),
            batch_count,
            cache_path.display(),
            all_ticks.time_range
        );
        
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
        .ok_or_else(|| DataError::InvalidInput("Missing 'open_time_us' column".to_string()))?
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .ok_or_else(|| DataError::InvalidInput("Invalid 'open_time_us' column type".to_string()))?;
    let open_array = batch
        .column_by_name("open")
        .ok_or_else(|| DataError::InvalidInput("Missing 'open' column".to_string()))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or_else(|| DataError::InvalidInput("Invalid 'open' column type".to_string()))?;
    let high_array = batch
        .column_by_name("high")
        .ok_or_else(|| DataError::InvalidInput("Missing 'high' column".to_string()))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or_else(|| DataError::InvalidInput("Invalid 'high' column type".to_string()))?;
    let low_array = batch
        .column_by_name("low")
        .ok_or_else(|| DataError::InvalidInput("Missing 'low' column".to_string()))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or_else(|| DataError::InvalidInput("Invalid 'low' column type".to_string()))?;
    let close_array = batch
        .column_by_name("close")
        .ok_or_else(|| DataError::InvalidInput("Missing 'close' column".to_string()))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or_else(|| DataError::InvalidInput("Invalid 'close' column type".to_string()))?;
    let volume_array = batch
        .column_by_name("volume")
        .ok_or_else(|| DataError::InvalidInput("Missing 'volume' column".to_string()))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or_else(|| DataError::InvalidInput("Invalid 'volume' column type".to_string()))?;
    let num_trades_array = batch
        .column_by_name("num_trades")
        .ok_or_else(|| DataError::InvalidInput("Missing 'num_trades' column".to_string()))?
        .as_any()
        .downcast_ref::<UInt32Array>()
        .ok_or_else(|| DataError::InvalidInput("Invalid 'num_trades' column type".to_string()))?;

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
        .ok_or_else(|| DataError::InvalidInput("Missing 'price' column".to_string()))?
        .as_any()
        .downcast_ref::<UInt32Array>()
        .ok_or_else(|| DataError::InvalidInput("Invalid 'price' column type".to_string()))?;
    let volume_array = batch
        .column_by_name("volume")
        .ok_or_else(|| DataError::InvalidInput("Missing 'volume' column".to_string()))?
        .as_any()
        .downcast_ref::<Float32Array>()
        .ok_or_else(|| DataError::InvalidInput("Invalid 'volume' column type".to_string()))?;

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
            .ok_or_else(|| DataError::InvalidInput("Invalid 'time_range_start_us' column type".to_string()))?;
        let end_array = end_col
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or_else(|| DataError::InvalidInput("Invalid 'time_range_end_us' column type".to_string()))?;
        
        // Read from first row (where time_range is stored)
        if start_array.len() > 0 && end_array.len() > 0 && start_array.is_valid(0) && end_array.is_valid(0) {
            let start_val = start_array.value(0);
            let end_val = end_array.value(0);
            
            // Check if values look like nanoseconds (too large for microseconds)
            // Microseconds for 2025 dates should be around 1.7e15, nanoseconds would be 1.7e18
            let (start_us, end_us) = if start_val > 1_000_000_000_000_000_000 {
                // Looks like nanoseconds, convert to microseconds
                (start_val / 1_000, end_val / 1_000)
            } else {
                (start_val, end_val)
            };
            
            Some((start_us, end_us))
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

impl HistoricalDownloadExecutor {
    /// Waits for a concurrent download to complete, or returns None if no concurrent download.
    async fn wait_for_concurrent_download_klines(
        &self,
        cache_path: &Path,
        download_key: &str,
    ) -> Option<Vec<KLine>> {
        let is_downloading = {
            let downloading = self.downloading.lock().await;
            downloading.contains(download_key)
        };

        if !is_downloading {
            return None;
        }

        // Wait for concurrent download to complete
        for _ in 0..30 {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            if cache_path.exists() {
                if let Ok(klines) = self.load_klines_from_cache(cache_path).await {
                    return Some(klines);
                }
            }
        }
        None
    }

    /// Waits for a concurrent download to complete, or returns None if no concurrent download.
    async fn wait_for_concurrent_download_ticks(
        &self,
        cache_path: &Path,
        download_key: &str,
    ) -> Option<TickDataBuffer> {
        let is_downloading = {
            let downloading = self.downloading.lock().await;
            downloading.contains(download_key)
        };

        if !is_downloading {
            return None;
        }

        // Wait for concurrent download to complete
        for _ in 0..30 {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            if cache_path.exists() {
                if let Ok(ticks) = self.load_ticks_from_cache(cache_path).await {
                    return Some(ticks);
                }
            }
        }
        None
    }
}

