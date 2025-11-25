//! Service for handling blocking I/O operations, specifically for reading and
//! aggregating live data from the memory-mapped store.

use std::sync::Arc;
use storage::MmapStore;
use arrow::array; // Required for downcasting Arrow arrays

use crate::{kline::KLine, data_error::DataError, compute::vp::TickDataBuffer, realtime_ingester::normalize_binance_symbol};

/// Defines a time range with microsecond precision.
/// Microsecond precision is sufficient for financial data and allows representing
/// timestamps up to year 584,542 (far beyond any practical need).
#[derive(Debug, Clone, Copy)]
pub struct TimeRange {
    /// Start of the time range, inclusive, in microseconds.
    pub start_us: u64,
    /// End of the time range, exclusive, in microseconds.
    pub end_us: u64,
}

/// A lightweight representation of a trade for aggregation.
#[derive(Debug, Clone, Copy)]
struct LightweightTrade {
    time: u64,
    price: f64,
    qty: f64,
    is_buy: bool,
}

/// Helper function to normalize key_hash to microseconds.
/// Detects and fixes timestamp unit issues (e.g., nanoseconds stored as microseconds).
/// A reasonable microsecond timestamp for 2025 would be around 1.7e15,
/// so anything above 1e18 is likely nanoseconds.
fn normalize_key_hash_to_us(key_hash: u64) -> u64 {
    if key_hash > 1_000_000_000_000_000_000 {
        // Likely nanoseconds, convert to microseconds
        key_hash / 1_000
    } else {
        key_hash
    }
}

/// A service dedicated to reading real-time data from memory-mapped files.
///
/// It can dynamically open MmapStore files for different symbols.
/// These methods are designed to be run within `tokio::task::spawn_blocking`
/// to avoid blocking the main async runtime.
#[derive(Clone)]
pub struct RealtimeDataService {
    data_dir: std::path::PathBuf,
    kline_cache: Option<std::sync::Arc<crate::kline_cache::KlineCache>>,
}

impl RealtimeDataService {
    /// Creates a new `RealtimeDataService`.
    ///
    /// # Arguments
    ///
    /// * `data_dir` - The base directory where MmapStore files are stored.
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        Self {
            data_dir,
            kline_cache: None,
        }
    }

    /// Sets the K-line cache for this service.
    /// This allows the service to read K-line data from cache instead of always fetching from API.
    pub fn set_kline_cache(&mut self, cache: std::sync::Arc<crate::kline_cache::KlineCache>) {
        self.kline_cache = Some(cache);
    }
    
    /// Opens the MmapStore for a given symbol.
    /// Returns None if the file doesn't exist or is invalid.
    /// 
    /// This method will retry opening the file a few times with short delays,
    /// as the file may be in the process of being created by RealtimeIngesterService.
    fn open_store_for_symbol(&self, symbol: &str) -> Option<Arc<MmapStore>> {
        use crate::realtime_ingester::normalize_binance_symbol;
        let normalized_symbol = normalize_binance_symbol(symbol);
        // RealtimeIngesterService creates files as {normalized_symbol}.mmap directly in data_dir
        let mmap_path = self.data_dir.join(format!("{}.mmap", normalized_symbol));
        
        // Retry logic: file may be in the process of being created
        const MAX_RETRIES: u32 = 5;
        const RETRY_DELAY_MS: u64 = 200;
        
        for attempt in 0..=MAX_RETRIES {
            match MmapStore::open(&mmap_path) {
                Ok(store) => {
                    let index_len = store.index().len();
                    if index_len > 0 || attempt == MAX_RETRIES {
                        // File exists and has data, or we've exhausted retries
                        return Some(Arc::new(store));
                    } else {
                        // File exists but index is empty, wait a bit and retry
                        if attempt < MAX_RETRIES {
                            std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
                        continue;
                    } else {
                        // Check if the error is due to file corruption
                        let error_msg = e.to_string();
                        let is_corrupted = error_msg.contains("truncated") || error_msg.contains("invalid");
                        
                        if is_corrupted && mmap_path.exists() {
                            // DISABLED: Auto-deletion logic is disabled per user request
                            // Previously, corrupted files were automatically deleted to allow Ingester to recreate them.
                            // This has been disabled to prevent unexpected file deletion.
                            log::warn!(
                                "RealtimeDataService: MmapStore file for {} appears to be corrupted at {:?}. \
                                Error: {}. Auto-deletion is disabled. Please manually delete the file if needed.",
                                symbol,
                                mmap_path,
                                e
                            );
                            
                            // Auto-deletion code removed - file will not be deleted automatically
                            // If you need to fix a corrupted file, manually delete it and let Ingester recreate it
                        } else {
                            log::warn!(
                                "RealtimeDataService: failed to open MmapStore for {} at {:?} after {} attempts: {}",
                                symbol,
                                mmap_path,
                                MAX_RETRIES + 1,
                                e
                            );
                        }
                        return None;
                    }
                }
            }
        }
        
        None
    }

    /// Fetches tick data for a given time range from the MmapStore and aggregates
    /// it into K-lines.
    ///
    /// This is a synchronous, CPU-intensive, and potentially blocking operation.
    /// It **must** be called within `tokio::task::spawn_blocking`.
    ///
    /// # Arguments
    ///
    /// * `range` - The time range for which to fetch and aggregate data.
    /// Fetches K-line data for a given symbol and time range.
    ///
    /// This is a synchronous, CPU-intensive, and potentially blocking operation.
    /// It **must** be called within `tokio::task::spawn_blocking`.
    ///
    /// # Arguments
    ///
    /// Fetches K-line data from Binance REST API.
    /// 
    /// This method directly fetches K-line data from Binance API instead of aggregating from trade data,
    /// providing much faster response times for K-line chart display.
    /// 
    /// # Arguments
    /// 
    /// * `symbol` - The trading pair symbol (e.g., "BTCUSDT", "SOLUSDT")
    /// * `range` - The time range for which to fetch K-line data
    /// * `timeframe` - The K-line interval (e.g., "1m", "5m", "1h")
    pub fn fetch_klines_blocking(&self, symbol: &str, range: TimeRange, timeframe: &str) -> Result<Vec<KLine>, DataError> {
        // Normalize symbol to Binance API format
        let api_symbol = normalize_binance_symbol(symbol);

        // 1. Try to get from cache first
        if let Some(cache) = &self.kline_cache {
            let cached_klines = cache.get_range(&api_symbol, timeframe, range.start_us, range.end_us);
            if !cached_klines.is_empty() {
                // Check if cache covers the requested range
                if let Some((cache_min, cache_max)) = cache.time_range(&api_symbol, timeframe) {
                    let cache_coverage = cache_min <= range.start_us && cache_max >= range.end_us;
                    if cache_coverage {
                        return Ok(cached_klines);
                    }
                }
            }
        }

        // 2. Cache miss or insufficient coverage: fetch from API
        // Binance K-line API endpoint
        let url = format!(
            "https://api.binance.com/api/v3/klines?symbol={}&interval={}&startTime={}&endTime={}&limit=1000",
            api_symbol,
            timeframe,
            range.start_us / 1_000, // Convert us to ms
            range.end_us / 1_000    // Convert us to ms
        );

        // Use blocking HTTP client
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| DataError::Network(reqwest::Error::from(e)))?;

        let response = client.get(&url).send()
            .map_err(|e| DataError::Network(e))?;

        if !response.status().is_success() {
            return Err(DataError::Network(
                response.error_for_status().unwrap_err()
            ));
        }

        // Parse JSON response
        // Binance returns an array of arrays: [[open_time, open, high, low, close, volume, close_time, ...], ...]
        let klines_json: Vec<Vec<serde_json::Value>> = response.json()
            .map_err(|e| DataError::InvalidInput("Failed to parse JSON response".to_string()))?;

        let mut klines = Vec::new();
        for kline_array in klines_json {
            if kline_array.len() < 9 {
                continue; // Skip invalid entries
        }

            // Parse fields from Binance response
            // [0] Open time (ms)
            // [1] Open price (string)
            // [2] High price (string)
            // [3] Low price (string)
            // [4] Close price (string)
            // [5] Volume (string)
            // [6] Close time (ms)
            // [7] Quote asset volume (string)
            // [8] Number of trades (u64)
            // [9] Taker buy base asset volume (string)
            // [10] Taker buy quote asset volume (string)
            // [11] Ignore

            let open_time_ms = kline_array[0].as_u64()
                .ok_or_else(|| DataError::InvalidInput("Invalid open_time".to_string()))?;
            
            let open = kline_array[1].as_str()
                .ok_or_else(|| DataError::InvalidInput("Invalid open price".to_string()))?
                .parse::<f64>()
                .map_err(|_| DataError::InvalidInput("Failed to parse open price".to_string()))?;
            
            let high = kline_array[2].as_str()
                .ok_or_else(|| DataError::InvalidInput("Invalid high price".to_string()))?
                .parse::<f64>()
                .map_err(|_| DataError::InvalidInput("Failed to parse high price".to_string()))?;
            
            let low = kline_array[3].as_str()
                .ok_or_else(|| DataError::InvalidInput("Invalid low price".to_string()))?
                .parse::<f64>()
                .map_err(|_| DataError::InvalidInput("Failed to parse low price".to_string()))?;
            
            let close = kline_array[4].as_str()
                .ok_or_else(|| DataError::InvalidInput("Invalid close price".to_string()))?
                .parse::<f64>()
                .map_err(|_| DataError::InvalidInput("Failed to parse close price".to_string()))?;
            
            let volume = kline_array[5].as_str()
                .ok_or_else(|| DataError::InvalidInput("Invalid volume".to_string()))?
                .parse::<f64>()
                .map_err(|_| DataError::InvalidInput("Failed to parse volume".to_string()))?;
            
            let num_trades = kline_array[8].as_u64()
                .ok_or_else(|| DataError::InvalidInput("Invalid num_trades".to_string()))?
                as u32;

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

        // Filter to requested range (Binance API may return slightly more data)
        klines.retain(|k| k.open_time_us >= range.start_us && k.open_time_us < range.end_us);

        // 3. Update cache with fetched data
        if let Some(cache) = &self.kline_cache {
            cache.insert_many(&api_symbol, timeframe, &klines);
        }

        Ok(klines)
    }

    /// Fetches raw tick data for a given time range from the MmapStore and converts it to TickDataBuffer.
    ///
    /// This is a synchronous, CPU-intensive, and potentially blocking operation.
    /// It **must** be called within `tokio::task::spawn_blocking`.
    ///
    /// # Arguments
    ///
    /// * `range` - The time range for which to fetch tick data.
    /// Fetches tick data for a given symbol and time range.
    ///
    /// This is a synchronous, CPU-intensive, and potentially blocking operation.
    /// It **must** be called within `tokio::task::spawn_blocking`.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The trading pair symbol (e.g., "BTCUSDT", "SOLUSDT")
    /// * `range` - The time range for which to fetch tick data.
    pub fn fetch_ticks_blocking(&self, symbol: &str, range: TimeRange) -> Result<TickDataBuffer, DataError> {
        let store = match self.open_store_for_symbol(symbol) {
            Some(store) => store,
            None => {
                log::warn!("RealtimeDataService: MmapStore not available for {}, returning empty result", symbol);
                return Ok(TickDataBuffer { 
                    prices: Vec::new(), 
                    volumes: Vec::new(),
                    time_range: None,
                });
            }
        };
        
        let index = store.index();
        

        // Find the first data block that *could* contain data for our time range.
        // partition_point returns the index where entry.key_hash() >= range.start_us
        // CRITICAL: Normalize key_hash to microseconds to handle old data with wrong units
        let start_idx = index.partition_point(|entry| {
            let normalized_key = normalize_key_hash_to_us(entry.key_hash());
            normalized_key < range.start_us
        });
        
        // IMPORTANT: We must check the previous chunk as well, because:
        // 1. The query range start might fall within the duration of the previous chunk
        // 2. key_hash is the chunk's START time, but the chunk contains data that extends beyond that
        // 3. If start_idx == index.len(), all chunks start before range.start_us, but the last chunk might contain our data
        let scan_start_idx = if start_idx == 0 {
            0
        } else {
            start_idx.saturating_sub(1)
        };
        
        if index.is_empty() {
            return Ok(TickDataBuffer { 
                prices: Vec::new(), 
                volumes: Vec::new(),
                time_range: None,
            });
        }
        
        if scan_start_idx < index.len() {
            let raw_key = index[scan_start_idx].key_hash();
            let normalized_key = normalize_key_hash_to_us(raw_key);
            if raw_key != normalized_key {
                log::warn!("fetch_ticks_blocking: Detected timestamp unit issue in chunk at index {}: raw={}, normalized={} us", 
                    scan_start_idx, raw_key, normalized_key);
            }
        }

        let mut prices: Vec<u32> = Vec::new();
        let mut volumes: Vec<f32> = Vec::new();
        let mut chunks_checked = 0;
        let mut chunks_with_data = 0;
        let mut ticks_before_filter = 0;
        let mut actual_start: Option<u64> = None;
        let mut actual_end: Option<u64> = None;

        // Iterate through index entries that overlap with the requested time range.
        for entry in &index[scan_start_idx..] {
            // CRITICAL: Normalize key_hash to microseconds to handle old data with wrong units
            let chunk_start_us = normalize_key_hash_to_us(entry.key_hash());
            
            // If the block's start time is already after our range ends, we can stop.
            if chunk_start_us >= range.end_us {
                break;
            }
            
            chunks_checked += 1;

            // Load and deserialize payload
            let payload = store.get_payload(entry);
            if payload.is_empty() {
                // Skip entries with empty payload (e.g., file truncated or data not yet written)
                continue;
            }
            
            // CRITICAL: Verify payload length matches index entry length
            // This ensures we don't read incomplete Parquet files that could cause "Corrupt footer" errors
            if payload.len() != entry.length as usize {
                log::warn!("fetch_ticks_blocking: chunk at {} us has mismatched length: index says {} bytes, actual payload is {} bytes. Skipping to avoid corrupt Parquet.", 
                    chunk_start_us, entry.length, payload.len());
                continue;
            }
            
            chunks_with_data += 1;
            let payload_bytes = bytes::Bytes::from(payload.to_vec());
            
            // Wrap Parquet parsing in a more descriptive error context
            let reader = match parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(payload_bytes) {
                Ok(builder) => builder.with_batch_size(8192).build(),
                Err(e) => {
                    log::warn!("fetch_ticks_blocking: Failed to create Parquet reader for chunk at {} us (length {} bytes): {}. This may indicate incomplete file write. Skipping.", 
                        chunk_start_us, payload.len(), e);
                    continue;
                }
            }?;

            for batch_result in reader {
                let batch = batch_result?;
                
                let timestamps = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<array::TimestampMicrosecondArray>()
                    .ok_or_else(|| DataError::InvalidInput("Timestamp column has wrong type".to_string()))?;
                let price_array = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<array::Float64Array>()
                    .ok_or_else(|| DataError::InvalidInput("Price column has wrong type".to_string()))?;
                let volume_array = batch
                    .column(2)
                    .as_any()
                    .downcast_ref::<array::Float64Array>()
                    .ok_or_else(|| DataError::InvalidInput("Volume column has wrong type".to_string()))?;

                for i in 0..batch.num_rows() {
                    let ts = timestamps.value(i) as u64;
                    ticks_before_filter += 1;
                    // Filter ticks to be within the requested range
                    // Filter ticks to be within the requested range
                    if ts >= range.start_us && ts < range.end_us {
                        // Convert price from f64 to u32 (fixed-point representation)
                        // Assuming price is in dollars with 2 decimal places, multiply by 100
                        // u32 max (~4.2 billion) represents prices up to ~42 million
                        let price_fixed = (price_array.value(i) * 100.0) as u32;
                        prices.push(price_fixed);
                        volumes.push(volume_array.value(i) as f32);
                        
                        // Track actual time range covered by the data
                        if actual_start.is_none() || ts < actual_start.unwrap() {
                            actual_start = Some(ts);
                        }
                        if actual_end.is_none() || ts > actual_end.unwrap() {
                            actual_end = Some(ts);
                        }
                    }
                }
            }
        }
        

        // Record the actual time range covered by the data
        let time_range = if let (Some(start), Some(end)) = (actual_start, actual_end) {
            Some((start, end))
        } else {
            None
        };

        Ok(TickDataBuffer { 
            prices, 
            volumes,
            time_range,
        })
    }
}
