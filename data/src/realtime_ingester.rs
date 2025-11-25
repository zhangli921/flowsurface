use arrow::array::{
    ArrayRef, BooleanArray, Float64Array, TimestampMicrosecondArray,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use exchange::{util::Price, Trade};
use futures_util::StreamExt;
use parquet::arrow::arrow_writer::ArrowWriter;
use reqwest::Client;
use serde::Deserialize;
use storage::{FileHeader, IndexEntry};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tokio::sync::mpsc;
use log::{info, error, warn};

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::mem;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::path::PathBuf;

use crate::kline_cache::KlineCache;
use crate::kline::KLine;

const MAGIC_NUMBER: &[u8; 8] = b"ZEROCPY!";
const DATA_VERSION: u16 = 1;
const INDEX_CAPACITY: usize = 200_000;

/// Convert ticker format (e.g., "ETH-USDT-SWAP") to Binance API format (e.g., "ETHUSDT").
/// This handles various ticker formats used in the UI and converts them to the format
/// expected by Binance REST API and WebSocket.
pub fn normalize_binance_symbol(ticker: &str) -> String {
    let ticker_upper = ticker.to_uppercase();
    
    // If already in Binance format (e.g., "BTCUSDT"), return as-is
    if ticker_upper.ends_with("USDT") || ticker_upper.ends_with("USD") || ticker_upper.ends_with("BUSD") {
        return ticker_upper.replace("-", "").replace("_", "");
    }
    
    // Remove common suffixes and separators
    let normalized = ticker_upper
        .replace("-USDT-SWAP", "")
        .replace("-USDT", "")
        .replace("-USD", "")
        .replace("_PERP", "")
        .replace("-", "")
        .replace("_", "");
    
    // If it still contains separators, try to extract base and quote
    if normalized.contains("-") {
        let parts: Vec<&str> = normalized.split('-').collect();
        if parts.len() >= 2 {
            return format!("{}{}", parts[0], parts[1]);
        }
    }
    
    // If it's a pure coin name (e.g., "BTC", "ETH"), add "USDT" suffix
    // This is the default quote currency for Binance spot trading
    if normalized.len() <= 10 && normalized.chars().all(|c| c.is_alphabetic()) {
        return format!("{}USDT", normalized);
    }
    
    normalized
} 

#[derive(Debug, Deserialize)]
struct AggTrade {
    #[serde(rename = "a")]
    _id: u64,
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "q")]
    qty: String,
    #[serde(rename = "T")]
    time: u64,
    #[serde(rename = "m")]
    is_buyer_maker: bool,
}

// Command enum for controlling the Realtime Ingestion Service
#[derive(Debug, Clone)]
pub enum IngestCommand {
    Subscribe(String, Option<String>), // Symbol, optional timeframe (e.g., "1m", "5m", "1h")
    Unsubscribe(String),
    Shutdown,
}

pub struct RealtimeIngesterService {
    command_rx: mpsc::Receiver<IngestCommand>,
    active_symbol: Option<String>,
    abort_handle: Option<tokio::task::JoinHandle<()>>,
    data_dir: PathBuf,
    kline_cache: Option<Arc<KlineCache>>,
}

impl RealtimeIngesterService {
    pub fn new(command_rx: mpsc::Receiver<IngestCommand>, data_dir: PathBuf) -> Self {
        Self {
            command_rx,
            active_symbol: None,
            abort_handle: None,
            data_dir,
            kline_cache: None,
        }
    }

    /// Sets the K-line cache for this service.
    /// This allows the service to cache K-line data for fast retrieval.
    pub fn set_kline_cache(&mut self, cache: Arc<KlineCache>) {
        self.kline_cache = Some(cache);
    }

    /// Synchronously creates the default Mmap file for a symbol.
    /// This should be called before starting the async run loop to ensure the file exists.
    /// CRITICAL: Uses normalized symbol to match the filename used in run_ingest_task.
    pub fn ensure_mmap_file(symbol: &str, data_dir: &PathBuf) {
        let normalized_symbol = normalize_binance_symbol(symbol);
        let mmap_path = data_dir.join(format!("{}.mmap", normalized_symbol));
        
        // Ensure parent directory exists
        if let Some(parent) = mmap_path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    error!("[ensure_mmap_file] FAILED to create parent directory {:?}: {}", parent, e);
                    return;
                }
            }
        }
        
        let mmap_path_str = match mmap_path.to_str() {
            Some(s) => s,
            None => {
                error!("[ensure_mmap_file] FAILED: Cannot convert path to string: {:?}", mmap_path);
                return;
            }
        };
        
        let (mut writer, _last_timestamp) = MmapWriter::open_or_create(mmap_path_str);
        writer.write_header();
        writer.write_indices();
        writer.flush_index(); // Ensure all pending index updates are written
        drop(writer); // Close the file
        
        // Verify file exists after creation
        if !mmap_path.exists() {
            error!("[ensure_mmap_file] FAILED: File does NOT exist after creation attempt: {:?}", mmap_path);
        }
    }

    pub async fn run(mut self) {
        info!("RealtimeIngesterService started.");
        
        // Auto-subscribe to BTCUSDT on startup (async download will happen in background)
        // No timeframe specified, will download all common timeframes
        info!("Auto-subscribing to BTCUSDT on startup...");
        self.switch_symbol("BTCUSDT", None).await;
        
        while let Some(cmd) = self.command_rx.recv().await {
            match cmd {
                IngestCommand::Subscribe(symbol, timeframe) => {
                    self.switch_symbol(&symbol, timeframe.as_deref()).await;
                }
                IngestCommand::Unsubscribe(symbol) => {
                     if self.active_symbol.as_deref() == Some(&symbol) {
                         info!("IngestCommand::Unsubscribe: {}", symbol);
                         self.stop_current_task();
                     }
                }
                IngestCommand::Shutdown => {
                    info!("RealtimeIngesterService shutting down.");
                    self.stop_current_task();
                    break;
                }
            }
        }
    }

    fn stop_current_task(&mut self) {
        if let Some(handle) = self.abort_handle.take() {
            handle.abort(); // Cancel the running task
        }
        self.active_symbol = None;
    }

    async fn switch_symbol(&mut self, symbol: &str, timeframe: Option<&str>) {
        if self.active_symbol.as_deref() == Some(symbol) {
            return; // Already active
        }
        
        self.stop_current_task();
        
        // Pre-create the Mmap file synchronously before starting the async task
        // This ensures the file exists immediately when VP computation tries to read it
        Self::ensure_mmap_file(symbol, &self.data_dir);
        
        let symbol_clone = symbol.to_string();
        let data_dir = self.data_dir.clone();
        let timeframe_clone = timeframe.map(|s| s.to_string());
        
        // Spawn a new task for this symbol
        let kline_cache_clone = self.kline_cache.clone();
        let handle = tokio::spawn(async move {
            run_ingest_task(symbol_clone, data_dir, kline_cache_clone, timeframe_clone).await;
        });
        
        self.abort_handle = Some(handle);
        self.active_symbol = Some(symbol.to_string());
    }
}

async fn run_ingest_task(symbol: String, data_dir: PathBuf, kline_cache: Option<Arc<KlineCache>>, timeframe: Option<String>) {
    // Normalize symbol for Binance API (e.g., "ETH-USDT-SWAP" -> "ETHUSDT", "BTC" -> "BTCUSDT")
    let api_symbol = normalize_binance_symbol(&symbol);
    
    // Use normalized symbol for filename to ensure consistency
    // This ensures VP computation can find the file using the same normalization
    let filename = format!("{}.mmap", api_symbol);
    let mmap_path = data_dir.join(filename);
    let mmap_path_str = mmap_path.to_str().unwrap();
    
    // Construct WS URL dynamically using normalized symbol
    let ws_url = format!("wss://stream.binance.com:9443/ws/{}@aggTrade", api_symbol.to_lowercase());

    // Start K-line cache update task if cache is available
    // Only download the specified timeframe if provided, otherwise download all common timeframes
    let kline_cache_clone = kline_cache.clone();
    let api_symbol_for_kline = api_symbol.clone();
    let timeframe_for_kline = timeframe.clone();
    if let Some(cache) = kline_cache_clone {
        tokio::spawn(async move {
            update_kline_cache_periodically(api_symbol_for_kline, cache, timeframe_for_kline).await;
        });
    }

    // 1. Initialize Mmap Writer
    let (mut writer, last_timestamp) = MmapWriter::open_or_create(mmap_path_str);
    
    // If file was newly created or recreated (last_timestamp is None), we need to write header and indices
    // This ensures the file has a valid structure even if it was empty or corrupted
    if last_timestamp.is_none() {
        writer.write_header();
        writer.write_indices();
    }
    
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    // Default 24h history to cover typical chart viewing ranges
    // Users often view charts with time ranges spanning several hours or even days
    let default_duration_hours = 24;
    
    let start_ms = if let Some(last_ts) = last_timestamp {
        info!("[{}] Resuming download from: {} ms (last timestamp in file)", symbol, last_ts);
        last_ts + 1 // Start from next ms to avoid duplication logic (or let append_chunk handle it)
    } else {
        let default_start = now_ms - (default_duration_hours * 60 * 60 * 1000);
        info!("[{}] Starting fresh download from: {} ms ({} hours ago)", symbol, default_start, default_duration_hours);
        default_start
    };
    
    info!("[{}] Downloading historical data: {} ms - {} ms (now)", symbol, start_ms, now_ms);

    if start_ms < now_ms {
        let client = Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
        let history_trades = fetch_binance_trades(&client, &api_symbol, start_ms, now_ms).await;

        let chunk_interval_us: u64 = 60 * 1_000_000; // 1 minute in microseconds
        if !history_trades.is_empty() {
            info!("[{}] Appending {} history trades...", symbol, history_trades.len());
            let mut current_chunk_start = history_trades[0].time - (history_trades[0].time % chunk_interval_us);
            let mut chunk_trades = Vec::new();

            for trade in history_trades {
                // Use checked_add to avoid overflow when calculating next chunk boundary
                if let Some(next_chunk_start) = current_chunk_start.checked_add(chunk_interval_us) {
                    if trade.time >= next_chunk_start {
                        writer.append_chunk(&chunk_trades, current_chunk_start);
                        chunk_trades.clear();
                        // Advance to the correct chunk for this trade
                        while let Some(next_start) = current_chunk_start.checked_add(chunk_interval_us) {
                            if trade.time >= next_start {
                                current_chunk_start = next_start;
                            } else {
                                break;
                            }
                        }
                    }
                } else {
                    // Chunk boundary overflow - this should never happen with valid timestamps
                    // but we handle it gracefully by stopping chunk processing
                    error!("[{}] CRITICAL: Chunk boundary overflow at time {} us. This indicates a serious timestamp issue. Stopping chunk processing.", symbol, trade.time);
                    break;
                }
                chunk_trades.push(trade);
            }
            if !chunk_trades.is_empty() {
                writer.append_chunk(&chunk_trades, current_chunk_start);
            }
        }
    }

    // 4. Live Streaming
    info!("[{}] Connecting to WebSocket: {}...", symbol, ws_url);
    let connect_res = connect_async(&ws_url).await;
    
    if let Err(e) = connect_res {
        error!("[{}] Failed to connect WS: {}", symbol, e);
        return;
    }
    
    let (ws_stream, _) = connect_res.unwrap();
    info!("[{}] Connected! Streaming...", symbol);

    let (_, mut read) = ws_stream.split();
    let mut live_buffer: Vec<Trade> = Vec::new();
    let mut last_flush_time = SystemTime::now();
    let flush_interval = Duration::from_secs(1);

    while let Some(message) = read.next().await {
        match message {
            Ok(Message::Text(text)) => {
                if let Ok(trade) = serde_json::from_str::<AggTrade>(&text) {
                    let price_f = trade.price.parse::<f32>().unwrap_or(0.0);
                    let qty_f = trade.qty.parse::<f32>().unwrap_or(0.0);
                    
                    // Convert ms to microseconds (us) for better range support
                    // Microsecond precision is sufficient for financial data (1us = 0.001ms)
                    let time_us = trade.time.checked_mul(1_000)
                        .unwrap_or_else(|| {
                            error!("CRITICAL: Time overflow detected in WebSocket stream! Timestamp {} ms * 1000 exceeds u64::MAX. This trade will be REJECTED to preserve data integrity.", trade.time);
                            u64::MAX // Will be filtered out below
                        });
                    if time_us == u64::MAX {
                        continue; // Skip this trade - data integrity is paramount
                    }
                    
                    live_buffer.push(Trade {
                        time: time_us, // Now in microseconds
                        is_sell: trade.is_buyer_maker,
                        price: Price::from_f32(price_f),
                        qty: qty_f,
                    });
                }
            }
            Ok(Message::Close(_)) => break,
            Err(e) => {
                error!("[{}] WS Error: {}", symbol, e);
                break; // Exit loop to restart? For now just exit task.
            },
            _ => {}
        }

        if last_flush_time.elapsed().unwrap() >= flush_interval {
            if !live_buffer.is_empty() {
                // Use first trade timestamp for chunk key - simple appending
                let chunk_time = live_buffer[0].time;
                writer.append_chunk(&live_buffer, chunk_time);
                live_buffer.clear();
            }
            last_flush_time = SystemTime::now();
        }
    }
    warn!("[{}] Ingestion task ended.", symbol);
}

/// Periodically updates K-line cache by fetching from Binance REST API.
/// This runs in the background and updates the cache every minute.
/// Only downloads the specified timeframe if provided, otherwise downloads all common timeframes.
async fn update_kline_cache_periodically(api_symbol: String, cache: Arc<KlineCache>, timeframe: Option<String>) {
    let client = Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
    let update_interval = Duration::from_secs(60); // Update every minute
    
    // Use specified timeframe if provided, otherwise use common timeframes
    let timeframes: Vec<String> = if let Some(tf) = timeframe {
        vec![tf]
    } else {
        vec!["1m".to_string(), "5m".to_string(), "15m".to_string(), "1h".to_string(), "4h".to_string(), "1d".to_string()]
    };
    
    loop {
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        // Fetch last 24 hours of data for each timeframe
        let start_ms = now_ms - (24 * 60 * 60 * 1000);
        
        for timeframe in timeframes.iter() {
            let url = format!(
                "https://api.binance.com/api/v3/klines?symbol={}&interval={}&startTime={}&endTime={}&limit=1000",
                api_symbol, timeframe, start_ms, now_ms
            );
            
            match client.get(&url).send().await {
                Ok(response) => {
                    if response.status().is_success() {
                        match response.json::<Vec<Vec<serde_json::Value>>>().await {
                            Ok(klines_json) => {
                                let mut klines = Vec::new();
                                for kline_array in klines_json {
                                    if kline_array.len() < 9 {
                                        continue;
                                    }
                                    
                                    if let (Some(open_time_ms), Some(open_str), Some(high_str), Some(low_str), Some(close_str), Some(volume_str), _, _, Some(num_trades)) = (
                                        kline_array[0].as_u64(),
                                        kline_array[1].as_str(),
                                        kline_array[2].as_str(),
                                        kline_array[3].as_str(),
                                        kline_array[4].as_str(),
                                        kline_array[5].as_str(),
                                        kline_array.get(6),
                                        kline_array.get(7),
                                        kline_array[8].as_u64(),
                                    ) {
                                        if let (Ok(open), Ok(high), Ok(low), Ok(close), Ok(volume)) = (
                                            open_str.parse::<f64>(),
                                            high_str.parse::<f64>(),
                                            low_str.parse::<f64>(),
                                            close_str.parse::<f64>(),
                                            volume_str.parse::<f64>(),
                                        ) {
                                            klines.push(KLine {
                                                open_time_us: open_time_ms * 1_000,
                                                open,
                                                high,
                                                low,
                                                close,
                                                volume,
                                                num_trades: num_trades as u32,
                                            });
                                        }
                                    }
                                }
                                
                                if !klines.is_empty() {
                                    cache.insert_many(&api_symbol, timeframe, &klines);
                                }
                            }
                            Err(e) => {
                                warn!("[{}] Failed to parse K-line JSON for {}: {}", api_symbol, timeframe, e);
                            }
                        }
                    } else {
                        warn!("[{}] Failed to fetch K-lines for {}: HTTP {}", api_symbol, timeframe, response.status());
                    }
                }
                Err(e) => {
                    warn!("[{}] Failed to fetch K-lines for {}: {}", api_symbol, timeframe, e);
                }
            }
        }
        
        tokio::time::sleep(update_interval).await;
    }
}

// --- Helpers from original main.rs ---

fn write_as_bytes<T>(writer: &mut impl Write, data: &T) -> std::io::Result<()> {
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (data as *const T) as *const u8,
            mem::size_of::<T>(),
        )
    };
    writer.write_all(bytes)
}

fn get_trades_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("timestamp_us", DataType::Timestamp(TimeUnit::Microsecond, None), false),
        Field::new("price", DataType::Float64, false),
        Field::new("volume", DataType::Float64, false), 
        Field::new("is_bid_aggressor", DataType::Boolean, false),
    ]))
}

fn trades_to_record_batch(trades: &[Trade]) -> Result<RecordBatch, arrow::error::ArrowError> {
    let schema = get_trades_schema();
    let mut timestamps = TimestampMicrosecondArray::builder(trades.len());
    let mut prices = Float64Array::builder(trades.len());
    let mut volumes = Float64Array::builder(trades.len());
    let mut is_bid_aggressors = BooleanArray::builder(trades.len());

    for trade in trades {
        timestamps.append_value(trade.time as i64);
        prices.append_value(trade.price.to_f32() as f64);
        volumes.append_value(trade.qty as f64);
        is_bid_aggressors.append_value(trade.is_sell);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(timestamps.finish()),
        Arc::new(prices.finish()),
        Arc::new(volumes.finish()),
        Arc::new(is_bid_aggressors.finish()),
    ];

    RecordBatch::try_new(schema, columns)
}

fn record_batch_to_parquet_bytes(batch: &RecordBatch) -> Result<Vec<u8>, parquet::errors::ParquetError> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut buffer, batch.schema(), None)?;
    writer.write(batch)?;
    writer.close()?;
    Ok(buffer)
}

async fn fetch_binance_trades(client: &Client, symbol: &str, start_time_ms: u64, end_time_ms: u64) -> Vec<Trade> {
    let mut trades = Vec::new();
    let mut current_start = start_time_ms;
    let mut consecutive_failures = 0;
    const MAX_CONSECUTIVE_FAILURES: u32 = 3;

    info!("Fetching trades for {} from {} to {}...", symbol, start_time_ms, end_time_ms);

    while current_start < end_time_ms {
        let url = format!(
            "https://api.binance.com/api/v3/aggTrades?symbol={}&startTime={}&limit=1000",
            symbol, current_start
        );

        match client.get(&url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    match resp.json::<Vec<AggTrade>>().await {
                        Ok(agg_trades) => {
                            consecutive_failures = 0; // Reset on success
                            if agg_trades.is_empty() {
                                break;
                            }
                            let last_time = agg_trades.last().unwrap().time;
                            if last_time <= current_start {
                                current_start += 1;
                            } else {
                                current_start = last_time + 1;
                            }

                            for at in agg_trades {
                                if at.time > end_time_ms {
                                    continue;
                                }
                                let price_f = at.price.parse::<f32>().unwrap_or(0.0);
                                let qty_f = at.qty.parse::<f32>().unwrap_or(0.0);
                                // Convert ms to microseconds (us) for better range support
                                // Microsecond precision is sufficient for financial data (1us = 0.001ms)
                                // This allows representing timestamps up to year 584,542 (far beyond any practical need)
                                let time_us = at.time.checked_mul(1_000)
                                    .unwrap_or_else(|| {
                                        error!("CRITICAL: Time overflow detected! Timestamp {} ms * 1000 exceeds u64::MAX. This trade will be REJECTED to preserve data integrity.", at.time);
                                        u64::MAX // Will be filtered out below
                                    });
                                if time_us == u64::MAX {
                                    continue; // Skip this trade - data integrity is paramount
                                }
                                trades.push(Trade {
                                    time: time_us, // Now in microseconds
                                    is_sell: at.is_buyer_maker,
                                    price: Price::from_f32(price_f),
                                    qty: qty_f,
                                });
                            }
                        }
                        Err(e) => { 
                            error!("JSON Error: {}", e); 
                            consecutive_failures += 1;
                            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                                warn!("[{}] Too many consecutive failures ({}), stopping history download. Will proceed with live streaming only.", symbol, consecutive_failures);
                                break;
                            }
                            tokio::time::sleep(Duration::from_secs(1)).await; 
                        }
                    }
                } else {
                    consecutive_failures += 1;
                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        warn!("[{}] Too many consecutive HTTP errors ({}), stopping history download. Will proceed with live streaming only.", symbol, consecutive_failures);
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            Err(e) => { 
                error!("Conn Error: {}", e); 
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    warn!("[{}] Too many consecutive connection errors ({}), stopping history download. Will proceed with live streaming only.", symbol, consecutive_failures);
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await; 
            }
        }
        // tokio::time::sleep(Duration::from_millis(10)).await; // Rate limit niceness
    }
    info!("Finished fetching {}. Total trades: {}", symbol, trades.len());
    trades
}

struct MmapWriter {
    file: File,
    index_entries: Vec<IndexEntry>,
    payload_start_offset: usize,
    current_payload_offset: usize,
    pending_index_updates: usize, // Track number of chunks since last index update
    last_index_update: std::time::Instant, // Track time since last index update
}

const INDEX_UPDATE_BATCH_SIZE: usize = 10; // Update index every N chunks
const INDEX_UPDATE_INTERVAL_SECS: u64 = 1; // Update index at least every N seconds

impl MmapWriter {
    fn open_or_create(path: &str) -> (Self, Option<u64>) {
        let header_size = mem::size_of::<FileHeader>();
        let index_size = INDEX_CAPACITY * mem::size_of::<IndexEntry>();
        let expected_payload_start = header_size + index_size;

        // Ensure directory exists
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    error!("[MmapWriter::open_or_create] FAILED to create parent directory {:?}: {}", parent, e);
                    panic!("Failed to create parent directory: {}", e);
                }
            }
        }

        let mut file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path) {
            Ok(f) => f,
            Err(e) => {
                error!("[MmapWriter::open_or_create] FAILED to open file {}: {}", path, e);
                panic!("Failed to open file: {}", e);
            }
        };

        let file_len = match file.metadata() {
            Ok(meta) => meta.len(),
            Err(e) => {
                error!("[MmapWriter::open_or_create] FAILED to get file metadata: {}", e);
                panic!("Failed to get file metadata: {}", e);
            }
        };
        let mut last_timestamp = None;

        let (writer, last_timestamp_from_file) = if file_len >= header_size as u64 {
            // Try to read existing file
            let mut header_buf = vec![0u8; header_size];
            file.read_exact(&mut header_buf).unwrap();
            
            let header = unsafe { &*(header_buf.as_ptr() as *const FileHeader) };
            
            if &header.magic_number == MAGIC_NUMBER && header.data_version == DATA_VERSION {
                let index_count = header.index_count;
                let mut index_entries = Vec::with_capacity(index_count);
                
                file.seek(SeekFrom::Start(header_size as u64)).unwrap();
                let mut index_buf = vec![0u8; index_count * mem::size_of::<IndexEntry>()];
                if let Ok(_) = file.read_exact(&mut index_buf) {
                    let entry_size = mem::size_of::<IndexEntry>();
                    for i in 0..index_count {
                        let offset = i * entry_size;
                        let entry = unsafe { &*(index_buf[offset..].as_ptr() as *const IndexEntry) };
                        index_entries.push(*entry);
                    }
                    
                    if let Some(last) = index_entries.last() {
                        // CRITICAL: Detect and fix timestamp unit issues
                        // If key_hash is suspiciously large (looks like nanoseconds instead of microseconds),
                        // convert it. A reasonable microsecond timestamp for 2025 would be around 1.7e15,
                        // so anything above 1e18 is likely nanoseconds.
                        let key_hash_us = if last.key_hash > 1_000_000_000_000_000_000 {
                            // Likely nanoseconds, convert to microseconds
                            warn!("Detected suspiciously large key_hash {} (likely nanoseconds), converting to microseconds", last.key_hash);
                            last.key_hash / 1_000
                        } else {
                            last.key_hash
                        };
                        last_timestamp = Some(key_hash_us / 1_000); // Convert us to ms
                    }
                    
                    let current_payload_offset = if let Some(last) = index_entries.last() {
                        last.start_offset + last.length as usize
                    } else {
                        0
                    };
                    
                    // CRITICAL: Validate that the file actually contains payload data
                    // If the file is too short or all entries have empty payload, the file is corrupted
                    let min_file_size = expected_payload_start + current_payload_offset;
                    let has_valid_payload = file_len >= min_file_size as u64 && 
                        index_entries.iter().any(|e| e.length > 0);
                    
                    if !has_valid_payload {
                        warn!("Existing file has invalid or empty payload (file_len={}, expected_min={}, entries_with_data={}). Re-creating file.", 
                            file_len, min_file_size, index_entries.iter().filter(|e| e.length > 0).count());
                        // Close the file and re-create it
                        drop(file);
                        let new_file = OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create(true)
                            .truncate(true)
                            .open(path)
                            .expect("Failed to re-create file");
                        let mut writer = Self::create_new(new_file, expected_payload_start);
                        // Write header and indices for the recreated file
                        writer.write_header();
                        writer.write_indices();
                        return (writer, None);
                    }
                    

                    (Self {
                        file,
                        index_entries,
                        payload_start_offset: expected_payload_start,
                        current_payload_offset,
                        pending_index_updates: 0,
                        last_index_update: std::time::Instant::now(),
                    }, last_timestamp)
                } else {
                    warn!("Failed to read indices, re-creating file.");
                    let mut writer = Self::create_new(file, expected_payload_start);
                    writer.write_header();
                    writer.write_indices();
                    (writer, None)
                }
            } else {
                warn!("Invalid magic/version, re-creating file.");
                let mut writer = Self::create_new(file, expected_payload_start);
                writer.write_header();
                writer.write_indices();
                (writer, None)
            }
        } else {
            let mut writer = Self::create_new(file, expected_payload_start);
            // For newly created files, we need to write header and indices immediately
            // to ensure the file has a valid structure
            writer.write_header();
            writer.write_indices();
            (writer, None)
        };
        
        (writer, last_timestamp_from_file)
    }
    
    fn create_new(mut file: File, payload_start: usize) -> Self {
        match file.set_len(0) {
            Ok(_) => {},
            Err(e) => {
                error!("[MmapWriter::create_new] FAILED to truncate file: {}", e);
                panic!("Failed to truncate file: {}", e);
            }
        }
        
        match file.seek(SeekFrom::Start(0)) {
            Ok(_) => {},
            Err(e) => {
                error!("[MmapWriter::create_new] FAILED to seek: {}", e);
                panic!("Failed to seek: {}", e);
            }
        }
        let writer = Self {
            file,
            index_entries: Vec::new(),
            payload_start_offset: payload_start,
            current_payload_offset: 0,
            pending_index_updates: 0,
            last_index_update: std::time::Instant::now(),
        };
        writer
    }

    fn write_header(&mut self) {
        match self.file.seek(SeekFrom::Start(0)) {
            Ok(_) => {},
            Err(e) => {
                error!("[MmapWriter::write_header] FAILED to seek: {}", e);
                panic!("Failed to seek: {}", e);
            }
        }
        let header = FileHeader {
            magic_number: *MAGIC_NUMBER,
            data_version: DATA_VERSION,
            index_count: self.index_entries.len(),
            payload_start_offset: self.payload_start_offset,
            reserved: [0; 38],
        };
        match write_as_bytes(&mut self.file, &header) {
            Ok(_) => {},
            Err(e) => {
                error!("[MmapWriter::write_header] FAILED to write header: {}", e);
                panic!("Failed to write header: {}", e);
            }
        }
    }

    fn write_indices(&mut self) {
        let header_size = mem::size_of::<FileHeader>();
        match self.file.seek(SeekFrom::Start(header_size as u64)) {
            Ok(_) => {},
            Err(e) => {
                error!("[MmapWriter::write_indices] FAILED to seek: {}", e);
                panic!("Failed to seek: {}", e);
            }
        }
        
        // Write all index entries
        for (idx, entry) in self.index_entries.iter().enumerate() {
            match write_as_bytes(&mut self.file, entry) {
                Ok(_) => {},
                Err(e) => {
                    error!("[MmapWriter::write_indices] FAILED to write index entry {}: {}", idx, e);
                    panic!("Failed to write index entry: {}", e);
                }
            }
        }
        
        // Write zero-filled entries for the remaining index capacity
        // This ensures the index region is complete and matches payload_start_offset
        let empty_entry = IndexEntry {
            key_hash: 0,
            start_offset: 0,
            length: 0,
            reserved: 0,
        };
        let remaining_entries = INDEX_CAPACITY - self.index_entries.len();
        for _ in 0..remaining_entries {
            write_as_bytes(&mut self.file, &empty_entry).unwrap();
        }
        
        // Ensure file is at least payload_start_offset bytes (header + index region)
        // This is required for MmapStore::open to work correctly
        // But don't truncate if file is larger (has payload data)
        let current_len = self.file.metadata().unwrap().len();
        let min_len = self.payload_start_offset as u64;
        if current_len < min_len {
            self.file.set_len(min_len).unwrap();
        }
        // Note: Don't sync here - let append_chunk handle syncing after all writes are complete
    }

    fn append_chunk(&mut self, trades: &[Trade], chunk_start_us: u64) {
        if trades.is_empty() { return; }
        
        if self.index_entries.len() >= INDEX_CAPACITY {
            error!("Index capacity reached! Cannot append more chunks.");
            return;
        }

        let record_batch = match trades_to_record_batch(trades) {
            Ok(batch) => batch,
            Err(e) => {
                error!("Failed to convert trades to record batch: {}", e);
                return;
            }
        };
        
        let payload = match record_batch_to_parquet_bytes(&record_batch) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to convert record batch to parquet: {}", e);
                return;
            }
        };

        let payload_file_offset = self.payload_start_offset + self.current_payload_offset;
        if let Err(e) = self.file.seek(SeekFrom::Start(payload_file_offset as u64)) {
            error!("Failed to seek to payload offset {}: {}", payload_file_offset, e);
            return;
        }
        
        if let Err(e) = self.file.write_all(&payload) {
            error!("Failed to write payload ({} bytes) at offset {}: {}", payload.len(), payload_file_offset, e);
            return;
        }
        
        // CRITICAL: Sync payload to disk before updating index
        // This ensures data integrity - if another thread opens MmapStore while we're writing,
        // it won't read incomplete Parquet files. We sync here (before updating index) so that
        // the payload is fully written to disk before the index points to it.
        if let Err(e) = self.file.sync_all() {
            error!("Failed to sync payload to disk: {}", e);
            return;
        }
        

        let existing_idx = self.index_entries.iter().position(|e| e.key_hash == chunk_start_us);

        if let Some(idx) = existing_idx {
            // Replace
             self.index_entries[idx] = IndexEntry {
                key_hash: chunk_start_us,
                start_offset: self.current_payload_offset,
                length: payload.len() as u32,
                reserved: 0,
            };
        } else {
            // New
            let index_entry = IndexEntry {
                key_hash: chunk_start_us,
                start_offset: self.current_payload_offset,
                length: payload.len() as u32,
                reserved: 0,
            };
            self.index_entries.push(index_entry);
        }

        self.current_payload_offset += payload.len();

        self.index_entries.sort_by_key(|e| e.key_hash);
        
        // Batch index updates: only update every N chunks or every N seconds
        self.pending_index_updates += 1;
        let should_update = self.pending_index_updates >= INDEX_UPDATE_BATCH_SIZE ||
            self.last_index_update.elapsed().as_secs() >= INDEX_UPDATE_INTERVAL_SECS;
        
        if should_update {
            self.write_indices();
            self.write_header();
            self.file.sync_all().unwrap();
            self.pending_index_updates = 0;
            self.last_index_update = std::time::Instant::now();
        }
        
        info!("Appended chunk: {} trades. Pending index updates: {}", trades.len(), self.pending_index_updates);
    }
    
    /// Force flush pending index updates to disk
    fn flush_index(&mut self) {
        if self.pending_index_updates > 0 {
            let pending_count = self.pending_index_updates;
            self.write_indices();
            self.write_header();
            self.file.sync_all().unwrap();
            self.pending_index_updates = 0;
            self.last_index_update = std::time::Instant::now();
        }
    }
}


