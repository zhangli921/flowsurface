use arrow::array::{
    ArrayRef, BooleanArray, Float64Array, TimestampNanosecondArray,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use exchange::{util::Price, Trade};
use parquet::arrow::arrow_writer::ArrowWriter;
use rand::prelude::*;
use storage::{FileHeader, IndexEntry};

use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::mem;
use std::sync::Arc;

const MAGIC_NUMBER: &[u8; 8] = b"ZEROCPY!";
const DATA_VERSION: u16 = 1;

/// Calculate a u64 hash for a given key.
fn calculate_hash<T: Hash>(t: &T) -> u64 {
    let mut s = DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
}

/// Helper to write raw struct data as bytes.
fn write_as_bytes<T>(writer: &mut impl Write, data: &T) -> std::io::Result<()> {
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (data as *const T) as *const u8,
            mem::size_of::<T>(),
        )
    };
    writer.write_all(bytes)
}

/// Generates a vector of random trades for demonstration purposes.
fn generate_dummy_trades(count: usize) -> Vec<Trade> {
    let mut rng = thread_rng();
    let mut trades = Vec::with_capacity(count);
    let start_time = 1732056000_000_000_000; // A timestamp in late 2025 (nanoseconds)
    let mut current_price = 70000.0;
    let time_increment_ns = 100_000_000; // 100 milliseconds apart, 10 ticks per second

    for i in 0..count {
        current_price += rng.gen_range(-50.0..50.0);
        trades.push(Trade {
            time: start_time + (i as u64 * time_increment_ns), // N nanoseconds apart
            is_sell: rng.gen_bool(0.5),
            price: Price::from_f32(current_price as f32),
            qty: rng.gen_range(0.01..1.0),
        });
    }
    trades
}

/// Defines the Arrow schema for the trade data.
fn get_trades_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("timestamp_ns", DataType::Timestamp(TimeUnit::Nanosecond, None), false),
        Field::new("price", DataType::Float64, false),
        Field::new("volume", DataType::Float64, false), // Using Float64 for qty
        Field::new("is_bid_aggressor", DataType::Boolean, false),
    ]))
}

/// Converts a slice of `Trade` objects into an Arrow `RecordBatch`.
fn trades_to_record_batch(trades: &[Trade]) -> Result<RecordBatch, arrow::error::ArrowError> {
    let schema = get_trades_schema();

    // Create builders for each column
    let mut timestamps = TimestampNanosecondArray::builder(trades.len());
    let mut prices = Float64Array::builder(trades.len());
    let mut volumes = Float64Array::builder(trades.len());
    let mut is_bid_aggressors = BooleanArray::builder(trades.len());

    for trade in trades {
        timestamps.append_value(trade.time as i64);
        prices.append_value(trade.price.to_f32() as f64);
        volumes.append_value(trade.qty as f64);
        // A sell order is an aggressor on the BID side of the book.
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

/// Serializes a `RecordBatch` into a Parquet byte vector.
fn record_batch_to_parquet_bytes(batch: &RecordBatch) -> Result<Vec<u8>, parquet::errors::ParquetError> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut buffer, batch.schema(), None)?;
    
    writer.write(batch)?;
    writer.close()?;
    
    Ok(buffer)
}


fn main() {
    println!("Generating dummy trade data...");
    let trades = generate_dummy_trades(24 * 60 * 60 * 10); // Approx 24 hours of 100ms interval trades

    println!("Processing trades into time-indexed blocks...");
    let mut all_index_entries: Vec<IndexEntry> = Vec::new();
    let mut all_parquet_payloads: Vec<Vec<u8>> = Vec::new();
    let mut current_payload_offset_in_payload_block: usize = 0;

    let chunk_interval_ns: u64 = 60 * 1_000_000_000; // 1 minute in nanoseconds
    let mut current_chunk_start_time_ns = trades[0].time;
    let mut chunk_trades = Vec::new();

    for trade in trades {
        if trade.time < current_chunk_start_time_ns + chunk_interval_ns {
            chunk_trades.push(trade);
        } else {
            // Process the current chunk
            if !chunk_trades.is_empty() {
                let record_batch = trades_to_record_batch(&chunk_trades)
                    .expect("Failed to create RecordBatch for chunk");
                let parquet_payload = record_batch_to_parquet_bytes(&record_batch)
                    .expect("Failed to serialize chunk to Parquet");

                let index_entry = IndexEntry {
                    key_hash: current_chunk_start_time_ns, // Use chunk start time as key
                    start_offset: current_payload_offset_in_payload_block,
                    length: parquet_payload.len() as u32,
                    reserved: 0,
                };
                all_index_entries.push(index_entry);
                current_payload_offset_in_payload_block += parquet_payload.len();
                all_parquet_payloads.push(parquet_payload);
            }

            // Start a new chunk
            current_chunk_start_time_ns = trade.time - (trade.time % chunk_interval_ns); // Align to interval boundary
            chunk_trades.clear();
            chunk_trades.push(trade);
        }
    }
    // Process the last chunk
    if !chunk_trades.is_empty() {
        let record_batch = trades_to_record_batch(&chunk_trades)
            .expect("Failed to create RecordBatch for last chunk");
        let parquet_payload = record_batch_to_parquet_bytes(&record_batch)
            .expect("Failed to serialize last chunk to Parquet");

        let index_entry = IndexEntry {
            key_hash: current_chunk_start_time_ns,
            start_offset: current_payload_offset_in_payload_block,
            length: parquet_payload.len() as u32,
            reserved: 0,
        };
        all_index_entries.push(index_entry);
        all_parquet_payloads.push(parquet_payload);
    }

    // Ensure index entries are sorted by key_hash (timestamp)
    all_index_entries.sort_by_key(|entry| entry.key_hash);

    let output_path = "test_data.mmap";
    println!("Writing data to '{}'...", output_path);
    let mut file = File::create(output_path).expect("Failed to create output file");

    let header_size = mem::size_of::<FileHeader>();
    let index_total_size = all_index_entries.len() * mem::size_of::<IndexEntry>();
    let payload_start_offset = header_size + index_total_size;

    // 1. Write Header
    let header = FileHeader {
        magic_number: *MAGIC_NUMBER,
        data_version: DATA_VERSION,
        index_count: all_index_entries.len(),
        payload_start_offset,
        reserved: [0; 38],
    };
    write_as_bytes(&mut file, &header).unwrap();

    // 2. Write Index Entries
    for entry in &all_index_entries {
        write_as_bytes(&mut file, entry).unwrap();
    }

    // 3. Write Payloads
    for payload in &all_parquet_payloads {
        file.write_all(payload).unwrap();
    }
    
    file.sync_all().unwrap();
    println!("Successfully wrote {} bytes to {}", file.metadata().unwrap().len(), output_path);
    println!("Generated {} index entries for {} chunks of data.", all_index_entries.len(), all_parquet_payloads.len());
}