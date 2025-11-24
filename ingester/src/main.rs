use arrow::array::{
    ArrayRef, BooleanArray, Float64Array, TimestampNanosecondArray,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use exchange::{util::Price, Trade};
use futures_util::StreamExt;
use parquet::arrow::arrow_writer::ArrowWriter;
use reqwest::Client; // Async Client
use serde::Deserialize;
use storage::{FileHeader, IndexEntry};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::mem;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

// Async version of fetch_binance_trades
async fn fetch_binance_trades(client: &Client, symbol: &str, start_time_ms: u64, end_time_ms: u64) -> Vec<Trade> {
    let mut trades = Vec::new();
    let mut current_start = start_time_ms;

    println!("Fetching trades for {} from {} to {}...", symbol, start_time_ms, end_time_ms);

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
                            
                            if trades.len() % 50_000 == 0 {
                                print!("\rFetched {} trades... ", trades.len());
                                // Flush stdout not straightforward in async, skipping or using println occasionally
                            }
                        }
                        Err(e) => { eprintln!("JSON Error: {}", e); tokio::time::sleep(Duration::from_secs(1)).await; }
                    }
        } else {
                    if resp.status().as_u16() == 429 { tokio::time::sleep(Duration::from_secs(60)).await; }
                    else { tokio::time::sleep(Duration::from_secs(1)).await; }
                }
            }
            Err(e) => { eprintln!("Conn Error: {}", e); tokio::time::sleep(Duration::from_secs(1)).await; }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    println!("\nFinished fetching. Total trades: {}", trades.len());
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
            println!("Found existing Mmap file, checking validity...");
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
                    
                    println!("Resuming from existing file. Entries: {}, Last Time: {:?}", index_count, last_timestamp);

                    Self {
                        file,
                        index_entries,
                        payload_start_offset: expected_payload_start,
                        current_payload_offset,
                    }
                } else {
                    println!("Failed to read indices, re-creating file.");
                    Self::create_new(file, expected_payload_start)
                }
            } else {
                println!("Invalid magic/version, re-creating file.");
                Self::create_new(file, expected_payload_start)
            }
        } else {
            println!("Creating new Mmap file...");
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
            eprintln!("Index capacity reached! Cannot append more chunks.");
            return;
        }

        let record_batch = trades_to_record_batch(trades).unwrap();
        let payload = record_batch_to_parquet_bytes(&record_batch).unwrap();

        let payload_file_offset = self.payload_start_offset + self.current_payload_offset;
        self.file.seek(SeekFrom::Start(payload_file_offset as u64)).unwrap();
        self.file.write_all(&payload).unwrap();

        let existing_idx = self.index_entries.iter().position(|e| e.key_hash == chunk_start_ns);

        if let Some(idx) = existing_idx {
            let old_len = self.index_entries[idx].length;
            println!("Replacing duplicate chunk for time {}. Old len: {}, New len: {}", chunk_start_ns, old_len, payload.len());
            
            self.index_entries[idx] = IndexEntry {
                key_hash: chunk_start_ns,
                start_offset: self.current_payload_offset,
                length: payload.len() as u32,
                reserved: 0,
            };
        } else {
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
        println!("Appended chunk: {} trades. Total Indices: {}", trades.len(), self.index_entries.len());
    }
}

#[tokio::main]
async fn main() {
    let symbol = "BTCUSDT";
    let ws_url = "wss://stream.binance.com:9443/ws/btcusdt@aggTrade";
    let mmap_path = "test_data.mmap";

    let (mut writer, last_timestamp) = MmapWriter::open_or_create(mmap_path);
    
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    let default_duration_hours = 24;
    
    let start_ms = if let Some(last_ts) = last_timestamp {
        println!("Resuming download from: {}", last_ts);
        last_ts
    } else {
        now_ms - (default_duration_hours * 60 * 60 * 1000)
    };

    if start_ms < now_ms {
        let client = Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
        // DIRECT AWAIT, NO THREAD SPAWNING
        let history_trades = fetch_binance_trades(&client, symbol, start_ms, now_ms).await;

        let chunk_interval_ns: u64 = 60 * 1_000_000_000; // 1 minute
        if !history_trades.is_empty() {
            println!("Appending history to Mmap...");
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
    } else {
        println!("Data is up to date.");
    }

    println!("Connecting to WebSocket: {}...", ws_url);
    let (ws_stream, _) = connect_async(ws_url).await.expect("Failed to connect");
    println!("Connected! Listening for live trades...");

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
            Err(e) => eprintln!("WS Error: {}", e),
            _ => {}
        }

        if last_flush_time.elapsed().unwrap() >= flush_interval {
            if !live_buffer.is_empty() {
                let chunk_time = live_buffer[0].time;
                writer.append_chunk(&live_buffer, chunk_time);
                live_buffer.clear();
}
            last_flush_time = SystemTime::now();
        }
    }
}
