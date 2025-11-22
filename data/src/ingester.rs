use arrow::array::{
    ArrayRef, BooleanArray, Float64Array, TimestampNanosecondArray,
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

const MAGIC_NUMBER: &[u8; 8] = b"ZEROCPY!";
const DATA_VERSION: u16 = 1;
const INDEX_CAPACITY: usize = 200_000; 

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

// Command enum for controlling the Ingestion Service
#[derive(Debug, Clone)]
pub enum IngestCommand {
    Subscribe(String), // Symbol
    Unsubscribe(String),
    Shutdown,
}

pub struct IngestionService {
    command_rx: mpsc::Receiver<IngestCommand>,
    active_symbol: Option<String>,
    abort_handle: Option<tokio::task::JoinHandle<()>>,
    data_dir: PathBuf,
}

impl IngestionService {
    pub fn new(command_rx: mpsc::Receiver<IngestCommand>, data_dir: PathBuf) -> Self {
        Self {
            command_rx,
            active_symbol: None,
            abort_handle: None,
            data_dir,
        }
    }

    pub async fn run(mut self) {
        info!("IngestionService started.");
        while let Some(cmd) = self.command_rx.recv().await {
            match cmd {
                IngestCommand::Subscribe(symbol) => {
                    info!("IngestCommand::Subscribe: {}", symbol);
                    self.switch_symbol(&symbol).await;
                }
                IngestCommand::Unsubscribe(symbol) => {
                     if self.active_symbol.as_deref() == Some(&symbol) {
                         info!("IngestCommand::Unsubscribe: {}", symbol);
                         self.stop_current_task();
                     }
                }
                IngestCommand::Shutdown => {
                    info!("IngestionService shutting down.");
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

    async fn switch_symbol(&mut self, symbol: &str) {
        if self.active_symbol.as_deref() == Some(symbol) {
            return; // Already active
        }
        
        self.stop_current_task();
        
        let symbol_clone = symbol.to_string();
        let data_dir = self.data_dir.clone();
        
        // Spawn a new task for this symbol
        let handle = tokio::spawn(async move {
            run_ingest_task(symbol_clone, data_dir).await;
        });
        
        self.abort_handle = Some(handle);
        self.active_symbol = Some(symbol.to_string());
    }
}

async fn run_ingest_task(symbol: String, data_dir: PathBuf) {
    info!("Starting ingestion task for {}", symbol);
    
    let filename = format!("{}.mmap", symbol);
    let mmap_path = data_dir.join(filename);
    let mmap_path_str = mmap_path.to_str().unwrap();
    
    // Construct WS URL dynamically
    let ws_url = format!("wss://stream.binance.com:9443/ws/{}@aggTrade", symbol.to_lowercase());

    // 1. Initialize Mmap Writer
    let (mut writer, last_timestamp) = MmapWriter::open_or_create(mmap_path_str);
    
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    // Default 2h history
    let default_duration_hours = 2;
    
    let start_ms = if let Some(last_ts) = last_timestamp {
        info!("[{}] Resuming download from: {}", symbol, last_ts);
        last_ts + 1 // Start from next ms to avoid duplication logic (or let append_chunk handle it)
    } else {
        now_ms - (default_duration_hours * 60 * 60 * 1000)
    };

    if start_ms < now_ms {
        let client = Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
        let history_trades = fetch_binance_trades(&client, &symbol, start_ms, now_ms).await;

        let chunk_interval_ns: u64 = 60 * 1_000_000_000; // 1 minute
        if !history_trades.is_empty() {
            info!("[{}] Appending {} history trades...", symbol, history_trades.len());
            let mut current_chunk_start = history_trades[0].time - (history_trades[0].time % chunk_interval_ns);
            let mut chunk_trades = Vec::new();

            for trade in history_trades {
                if trade.time >= current_chunk_start + chunk_interval_ns {
                    writer.append_chunk(&chunk_trades, current_chunk_start);
                    chunk_trades.clear();
                    while trade.time >= current_chunk_start + chunk_interval_ns {
                        current_chunk_start += chunk_interval_ns;
                    }
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
                    
                    live_buffer.push(Trade {
                        time: trade.time * 1_000_000,
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
        Field::new("timestamp_ns", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("price", DataType::Float64, false),
        Field::new("volume", DataType::Float64, false), 
        Field::new("is_bid_aggressor", DataType::Boolean, false),
    ]))
}

fn trades_to_record_batch(trades: &[Trade]) -> Result<RecordBatch, arrow::error::ArrowError> {
    let schema = get_trades_schema();
    let mut timestamps = TimestampNanosecondArray::builder(trades.len());
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
                                trades.push(Trade {
                                    time: at.time * 1_000_000,
                                    is_sell: at.is_buyer_maker,
                                    price: Price::from_f32(price_f),
                                    qty: qty_f,
                                });
                            }
                        }
                        Err(e) => { error!("JSON Error: {}", e); tokio::time::sleep(Duration::from_secs(1)).await; }
                    }
                } else {
                     tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            Err(e) => { error!("Conn Error: {}", e); tokio::time::sleep(Duration::from_secs(1)).await; }
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
}

impl MmapWriter {
    fn open_or_create(path: &str) -> (Self, Option<u64>) {
        let header_size = mem::size_of::<FileHeader>();
        let index_size = INDEX_CAPACITY * mem::size_of::<IndexEntry>();
        let expected_payload_start = header_size + index_size;

        // Ensure directory exists
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .expect("Failed to open file");

        let file_len = file.metadata().unwrap().len();
        let mut last_timestamp = None;

        let mut writer = if file_len >= header_size as u64 {
            // Try to read existing file
            info!("Found existing Mmap file {}, checking validity...", path);
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
                        last_timestamp = Some(last.key_hash / 1_000_000); 
                    }
                    
                    let current_payload_offset = if let Some(last) = index_entries.last() {
                        last.start_offset + last.length as usize
                    } else {
                        0
                    };
                    
                    info!("Resuming from existing file. Entries: {}, Last Time: {:?}", index_count, last_timestamp);

                    Self {
                        file,
                        index_entries,
                        payload_start_offset: expected_payload_start,
                        current_payload_offset,
                    }
                } else {
                    warn!("Failed to read indices, re-creating file.");
                    Self::create_new(file, expected_payload_start)
                }
            } else {
                warn!("Invalid magic/version, re-creating file.");
                Self::create_new(file, expected_payload_start)
            }
        } else {
            info!("Creating new Mmap file {}...", path);
            Self::create_new(file, expected_payload_start)
        };
        
        (writer, last_timestamp)
    }
    
    fn create_new(mut file: File, payload_start: usize) -> Self {
        file.set_len(0).unwrap(); 
        file.seek(SeekFrom::Start(0)).unwrap();
        
        Self {
            file,
            index_entries: Vec::new(),
            payload_start_offset: payload_start,
            current_payload_offset: 0,
        }
    }

    fn write_header(&mut self) {
        self.file.seek(SeekFrom::Start(0)).unwrap();
        let header = FileHeader {
            magic_number: *MAGIC_NUMBER,
            data_version: DATA_VERSION,
            index_count: self.index_entries.len(),
            payload_start_offset: self.payload_start_offset,
            reserved: [0; 38],
        };
        write_as_bytes(&mut self.file, &header).unwrap();
    }

    fn write_indices(&mut self) {
        let header_size = mem::size_of::<FileHeader>();
        self.file.seek(SeekFrom::Start(header_size as u64)).unwrap();
        for entry in &self.index_entries {
            write_as_bytes(&mut self.file, entry).unwrap();
        }
    }

    fn append_chunk(&mut self, trades: &[Trade], chunk_start_ns: u64) {
        if trades.is_empty() { return; }
        
        if self.index_entries.len() >= INDEX_CAPACITY {
            error!("Index capacity reached! Cannot append more chunks.");
            return;
        }

        let record_batch = trades_to_record_batch(trades).unwrap();
        let payload = record_batch_to_parquet_bytes(&record_batch).unwrap();

        let payload_file_offset = self.payload_start_offset + self.current_payload_offset;
        self.file.seek(SeekFrom::Start(payload_file_offset as u64)).unwrap();
        self.file.write_all(&payload).unwrap();

        let existing_idx = self.index_entries.iter().position(|e| e.key_hash == chunk_start_ns);

        if let Some(idx) = existing_idx {
            // Replace
             self.index_entries[idx] = IndexEntry {
                key_hash: chunk_start_ns,
                start_offset: self.current_payload_offset,
                length: payload.len() as u32,
                reserved: 0,
            };
        } else {
            // New
            let index_entry = IndexEntry {
                key_hash: chunk_start_ns,
                start_offset: self.current_payload_offset,
                length: payload.len() as u32,
                reserved: 0,
            };
            self.index_entries.push(index_entry);
        }

        self.current_payload_offset += payload.len();

        self.index_entries.sort_by_key(|e| e.key_hash);
        
        self.write_indices();
        self.write_header();
        
        self.file.sync_all().unwrap();
        info!("Appended chunk: {} trades.", trades.len());
    }
}

