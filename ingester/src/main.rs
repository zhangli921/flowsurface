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
    let start_time = 1732056000_000_000_000; // A timestamp in late 2025
    let mut current_price = 70000.0;

    for i in 0..count {
        current_price += rng.gen_range(-50.0..50.0);
        trades.push(Trade {
            time: start_time + (i as u64 * 1_000_000_000), // 1 second apart
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
    let trades = generate_dummy_trades(1000);

    println!("Converting trades to Arrow RecordBatch...");
    let record_batch = trades_to_record_batch(&trades).expect("Failed to create RecordBatch");

    println!("Serializing RecordBatch to Parquet format...");
    let parquet_payload = record_batch_to_parquet_bytes(&record_batch).expect("Failed to serialize to Parquet");

    let output_path = "test_data.mmap";
    println!("Writing data to '{}'...", output_path);
    let mut file = File::create(output_path).expect("Failed to create output file");

    // For this example, we'll create one index entry for the whole payload.
    let payload_key = "trades/BTCUSDT/2025-11-20";
    let key_hash = calculate_hash(&payload_key);
    
    let header_size = mem::size_of::<FileHeader>();
    let index_size = mem::size_of::<IndexEntry>();
    let payload_start_offset = header_size + index_size;

    // 1. Write Header
    let header = FileHeader {
        magic_number: *MAGIC_NUMBER,
        data_version: DATA_VERSION,
        index_count: 1,
        payload_start_offset,
        reserved: [0; 38],
    };
    write_as_bytes(&mut file, &header).unwrap();

    // 2. Write Index
    let index_entry = IndexEntry {
        key_hash,
        start_offset: 0, // Offset is relative to the payload block
        length: parquet_payload.len() as u32,
        reserved: 0,
    };
    write_as_bytes(&mut file, &index_entry).unwrap();

    // 3. Write Payload
    file.write_all(&parquet_payload).unwrap();
    
    file.sync_all().unwrap();
    println!("Successfully wrote {} bytes to {}", file.metadata().unwrap().len(), output_path);
}