//! Unified data service that automatically selects between real-time and historical data sources.
//!
//! This service provides a unified interface for fetching K-line and Tick data,
//! automatically choosing between Speed Layer (real-time) and Batch Layer (historical)
//! based on the requested time range and the dynamic time boundary (safe_cutoff).

use std::sync::Arc;
use tokio::task::JoinHandle;
use chrono::{TimeZone, Utc};

use crate::{
    data_error::DataError,
    realtime_data_service::{RealtimeDataService, TimeRange},
    historical_data_service::HistoricalDataService,
    kline::KLine,
    compute::vp::TickDataBuffer,
};

/// Unified data service that automatically selects data sources.
///
/// This service encapsulates the logic for choosing between real-time and historical
/// data sources based on the requested time range and the dynamic time boundary.
pub struct UnifiedDataService {
    speed_layer: Arc<RealtimeDataService>,      // Real-time data reading service (Mmap access)
    batch_layer: Arc<HistoricalDataService>,    // Historical data reading service (cache access)
}

impl UnifiedDataService {
    /// Creates a new `UnifiedDataService`.
    pub fn new(
        speed_layer: Arc<RealtimeDataService>,
        batch_layer: Arc<HistoricalDataService>,
    ) -> Self {
        Self {
            speed_layer,
            batch_layer,
        }
    }

    /// Fetches K-line data, automatically selecting the data source.
    ///
    /// # Arguments
    /// - `symbol`: Trading pair identifier (e.g., "BTCUSDT")
    /// - `range`: Time range (microseconds)
    /// - `timeframe`: Timeframe (e.g., "1m", "5m", "1h")
    ///
    /// # Returns
    /// - `Ok(Vec<KLine>)`: K-line data
    /// - `Err(DataError)`: Error information
    pub async fn fetch_klines(
        &self,
        symbol: String,
        range: TimeRange,
        timeframe: &str, // e.g., "1m", "5m", "1h"
    ) -> Result<Vec<KLine>, DataError> {
        let safe_cutoff = calculate_safe_historical_cutoff();

        if range.start_us >= safe_cutoff {
            // Pure real-time data: read from Mmap and aggregate to K-lines
            tokio::task::spawn_blocking({
                let service = self.speed_layer.clone();
                move || service.fetch_kline_blocking(range)
            }).await?
        } else if range.end_us < safe_cutoff {
            // Pure historical data: download or read from cache
            self.batch_layer.fetch_kline(&symbol, range, timeframe).await
        } else {
            // Cross-boundary: fetch separately and merge
            let historical_future = self.batch_layer.fetch_kline(
                &symbol,
                TimeRange {
                    start_us: range.start_us,
                    end_us: safe_cutoff,
                },
                timeframe,
            );
            let realtime_future = tokio::task::spawn_blocking({
                let service = self.speed_layer.clone();
                move || service.fetch_kline_blocking(TimeRange {
                    start_us: safe_cutoff,
                    end_us: range.end_us,
                })
            });

            // Await both futures concurrently
            let (historical_result, realtime_join_result) = tokio::join!(
                historical_future,
                realtime_future
            );

            // Merge historical and real-time data
            let mut historical = historical_result?;
            let realtime = realtime_join_result??;

            // Combine and sort by timestamp
            historical.extend(realtime);
            historical.sort_by_key(|k| k.open_time_us);
            historical.dedup_by_key(|k| k.open_time_us);

            Ok(historical)
        }
    }

    /// Fetches Tick data, automatically selecting the data source.
    ///
    /// # Arguments
    /// - `symbol`: Trading pair identifier (e.g., "BTCUSDT")
    /// - `range`: Time range (microseconds)
    ///
    /// # Returns
    /// - `Ok(TickDataBuffer)`: Tick data
    /// - `Err(DataError)`: Error information
    pub async fn fetch_ticks(
        &self,
        symbol: String,
        range: TimeRange,
    ) -> Result<TickDataBuffer, DataError> {
        let safe_cutoff = calculate_safe_historical_cutoff();

        if range.start_us >= safe_cutoff {
            // Pure real-time data range (>= safe_cutoff)
            tokio::task::spawn_blocking({
                let service = self.speed_layer.clone();
                move || service.fetch_ticks_blocking(range)
            }).await?
        } else if range.end_us < safe_cutoff {
            // Pure historical data range (< safe_cutoff)
            self.batch_layer.fetch_ticks(&symbol, range).await
        } else {
            // Cross-boundary: query range spans safe_cutoff
            let historical_future = self.batch_layer.fetch_ticks(
                &symbol,
                TimeRange {
                    start_us: range.start_us,
                    end_us: safe_cutoff,
                },
            );
            let realtime_future = tokio::task::spawn_blocking({
                let service = self.speed_layer.clone();
                move || service.fetch_ticks_blocking(TimeRange {
                    start_us: safe_cutoff,
                    end_us: range.end_us,
                })
            });

            // Await both futures concurrently
            let (historical_result, realtime_join_result) = tokio::join!(
                historical_future,
                realtime_future
            );

            // Merge historical and real-time data
            let historical = historical_result?;
            let realtime = realtime_join_result??;

            // Merge TickDataBuffer
            // Note: prices and volumes must maintain one-to-one correspondence
            // Historical data first (earlier time), real-time data last (later time)
            Ok(TickDataBuffer {
                prices: {
                    let mut p = historical.prices;
                    p.extend(realtime.prices);
                    p
                },
                volumes: {
                    let mut v = historical.volumes;
                    v.extend(realtime.volumes);
                    v
                },
            })
        }
    }
}

/// Calculates the safe historical data cutoff time.
///
/// This function considers the Binance Data Vision publication delay (typically 2-6 hours)
/// and returns a timestamp that represents the boundary between real-time and historical data.
fn calculate_safe_historical_cutoff() -> u64 {
    let now = Utc::now();
    let today = now.date_naive();
    let midnight = today.and_hms_opt(0, 0, 0).unwrap();
    let cutoff = midnight.and_utc().timestamp_micros() as u64;

    // If current time is less than 6 hours since UTC midnight, use previous day's boundary
    let hours_since_midnight = (now.timestamp_micros() as u64 - cutoff) / 3_600_000_000;
    if hours_since_midnight < 6 {
        // Use previous day's boundary (historical data may not be published yet)
        let yesterday = today.pred_opt().unwrap_or(today);
        let yesterday_midnight = yesterday.and_hms_opt(0, 0, 0).unwrap();
        yesterday_midnight.and_utc().timestamp_micros() as u64
    } else {
        // Use today's boundary (historical data should be published)
        cutoff
    }
}

