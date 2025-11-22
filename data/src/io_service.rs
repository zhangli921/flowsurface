//! Service for handling blocking I/O operations, specifically for reading and
//! aggregating live data from the memory-mapped store.

use std::sync::Arc;
use storage::MmapStore;
use arrow::array; // Required for downcasting Arrow arrays

use crate::{kline::KLine, arbiter_error::ArbiterError, compute::vp::TickDataBuffer};

/// Defines a time range with nanosecond precision.
#[derive(Debug, Clone, Copy)]
pub struct TimeRange {
    /// Start of the time range, inclusive.
    pub start_ns: u64,
    /// End of the time range, exclusive.
    pub end_ns: u64,
}

/// A lightweight representation of a trade for aggregation.
#[derive(Debug, Clone, Copy)]
struct LightweightTrade {
    time: u64,
    price: f64,
    qty: f64,
    is_buy: bool,
}

/// A service dedicated to handling blocking I/O tasks.
///
/// It holds a reference to the `MmapStore` and provides methods to fetch
/// and process data from it. These methods are designed to be run within

/// `tokio::task::spawn_blocking` to avoid blocking the main async runtime.
#[derive(Clone)]
pub struct IoService {
    store: Arc<MmapStore>,
}

impl IoService {
    /// Creates a new `IoService`.
    ///
    /// # Arguments
    ///
    /// * `store` - An `Arc`-wrapped `MmapStore` instance.
    pub fn new(store: Arc<MmapStore>) -> Self {
        Self { store }
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
    pub fn fetch_live_kline_blocking(&self, range: TimeRange) -> Result<Vec<KLine>, ArbiterError> {
        let index = self.store.index();

        // Find the first data block that *could* contain data for our time range.
        let start_idx = index.partition_point(|entry| entry.key_hash() < range.start_ns);

        let mut all_trades: Vec<LightweightTrade> = Vec::new();

        // Iterate through index entries that overlap with the requested time range.
        for entry in &index[start_idx..] {
            // If the block's start time is already after our range ends, we can stop.
            if entry.key_hash() >= range.end_ns {
                break;
            }

            // Load and deserialize payload
            let payload = self.store.get_payload(entry);
            let payload_bytes = bytes::Bytes::from(payload.to_vec());
            
            let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(payload_bytes)?
                .with_batch_size(8192)
                .build()?;

            for batch_result in reader {
                let batch = batch_result?;
                
                let timestamps = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<array::TimestampNanosecondArray>()
                    .ok_or(ArbiterError::InvalidInput("Timestamp column has wrong type"))?;
                let prices = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<array::Float64Array>()
                    .ok_or(ArbiterError::InvalidInput("Price column has wrong type"))?;
                let volumes = batch
                    .column(2)
                    .as_any()
                    .downcast_ref::<array::Float64Array>()
                    .ok_or(ArbiterError::InvalidInput("Volume column has wrong type"))?;
                let is_bid_aggressors = batch
                    .column(3)
                    .as_any()
                    .downcast_ref::<array::BooleanArray>()
                    .ok_or(ArbiterError::InvalidInput("IsBidAggressor column has wrong type"))?;

                for i in 0..batch.num_rows() {
                    let ts = timestamps.value(i) as u64;
                    // Filter ticks to be within the requested range
                    if ts >= range.start_ns && ts < range.end_ns {
                        all_trades.push(LightweightTrade {
                            time: ts,
                            price: prices.value(i),
                            qty: volumes.value(i),
                            is_buy: !is_bid_aggressors.value(i), // If it's not a bid aggressor, it's a buy (taker)
                        });
                    }
                }
            }
        }

        // Sort trades by time, just in case (though ingester should provide them sorted within batches)
        all_trades.sort_by_key(|t| t.time);

        // --- K-line Aggregation ---
        const AGGREGATION_INTERVAL_NS: u64 = 60 * 1_000_000_000; // 1 minute in nanoseconds
        let mut klines: Vec<KLine> = Vec::new();
        let mut current_kline: Option<KLine> = None;

        for trade in all_trades {
            // Calculate the K-line's open_time_ns for this trade
            let kline_open_time_ns = (trade.time / AGGREGATION_INTERVAL_NS) * AGGREGATION_INTERVAL_NS;

            if let Some(mut kline) = current_kline {
                if kline.open_time_ns == kline_open_time_ns {
                    // Update existing K-line
                    kline.high = kline.high.max(trade.price);
                    kline.low = kline.low.min(trade.price);
                    kline.close = trade.price; // Update close price with every new trade
                    kline.volume += trade.qty;
                    kline.num_trades += 1;
                    current_kline = Some(kline);
                } else {
                    // New K-line interval, finalize the current one
                    klines.push(kline);
                    // Start a new K-line
                    current_kline = Some(KLine {
                        open_time_ns: kline_open_time_ns,
                        open: trade.price,
                        high: trade.price,
                        low: trade.price,
                        close: trade.price,
                        volume: trade.qty,
                        num_trades: 1,
                    });
                }
            } else {
                // First trade, start the first K-line
                current_kline = Some(KLine {
                    open_time_ns: kline_open_time_ns,
                    open: trade.price,
                    high: trade.price,
                    low: trade.price,
                    close: trade.price,
                    volume: trade.qty,
                    num_trades: 1,
                });
            }
        }

        // Push the last K-line if it exists
        if let Some(kline) = current_kline {
            klines.push(kline);
        }

        // Filter klines to match the exact requested range, as some might spill over
        klines.retain(|k| k.open_time_ns >= range.start_ns && k.open_time_ns < range.end_ns);

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
    pub fn fetch_ticks_blocking(&self, range: TimeRange) -> Result<TickDataBuffer, ArbiterError> {
        let index = self.store.index();

        // Find the first data block that *could* contain data for our time range.
        // partition_point returns the index where entry.key_hash() >= range.start_ns
        let start_idx = index.partition_point(|entry| entry.key_hash() < range.start_ns);
        
        // IMPORTANT: We must check the previous chunk as well, because:
        // 1. The query range start might fall within the duration of the previous chunk
        // 2. key_hash is the chunk's START time, but the chunk contains data that extends beyond that
        // 3. If start_idx == index.len(), all chunks start before range.start_ns, but the last chunk might contain our data
        let scan_start_idx = if start_idx == 0 {
            0
        } else {
            start_idx.saturating_sub(1)
        };

        let mut prices: Vec<u32> = Vec::new();
        let mut volumes: Vec<f32> = Vec::new();

        // Iterate through index entries that overlap with the requested time range.
        for entry in &index[scan_start_idx..] {
            // If the block's start time is already after our range ends, we can stop.
            if entry.key_hash() >= range.end_ns {
                break;
            }

            // Load and deserialize payload
            let payload = self.store.get_payload(entry);
            let payload_bytes = bytes::Bytes::from(payload.to_vec());
            
            let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(payload_bytes)?
                .with_batch_size(8192)
                .build()?;

            for batch_result in reader {
                let batch = batch_result?;
                
                let timestamps = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<array::TimestampNanosecondArray>()
                    .ok_or(ArbiterError::InvalidInput("Timestamp column has wrong type"))?;
                let price_array = batch
                    .column(1)
                    .as_any()
                    .downcast_ref::<array::Float64Array>()
                    .ok_or(ArbiterError::InvalidInput("Price column has wrong type"))?;
                let volume_array = batch
                    .column(2)
                    .as_any()
                    .downcast_ref::<array::Float64Array>()
                    .ok_or(ArbiterError::InvalidInput("Volume column has wrong type"))?;

                for i in 0..batch.num_rows() {
                    let ts = timestamps.value(i) as u64;
                    // Filter ticks to be within the requested range
                    if ts >= range.start_ns && ts < range.end_ns {
                        // Convert price from f64 to u32 (fixed-point representation)
                        // Assuming price is in dollars with 2 decimal places, multiply by 100
                        // u32 max (~4.2 billion) represents prices up to ~42 million
                        let price_fixed = (price_array.value(i) * 100.0) as u32;
                        prices.push(price_fixed);
                        volumes.push(volume_array.value(i) as f32);
                    }
                }
            }
        }

        Ok(TickDataBuffer { prices, volumes })
    }
}
