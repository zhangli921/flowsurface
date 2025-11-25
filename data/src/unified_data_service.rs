//! Unified data service that automatically selects between real-time and historical data sources.
//!
//! This service provides a unified interface for fetching K-line and Tick data,
//! automatically choosing between Speed Layer (real-time) and Batch Layer (historical)
//! based on the requested time range and the dynamic time boundary (safe_cutoff).

use std::sync::Arc;
use chrono::Utc;

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
        
        // log::info!(
        //     "UnifiedDataService: fetch_klines for {} timeframe {}, range: {} - {} us, safe_cutoff: {} us",
        //     symbol,
        //     timeframe,
        //     range.start_us,
        //     range.end_us,
        //     safe_cutoff
        // );

        if range.start_us >= safe_cutoff {
            // Pure real-time data: read from Mmap and aggregate to K-lines
            // log::info!("UnifiedDataService: attempting real-time data source (Mmap)");
            let symbol_clone = symbol.clone();
            let timeframe_clone = timeframe.to_string();
            let result = tokio::task::spawn_blocking({
                let service = self.speed_layer.clone();
                move || service.fetch_klines_blocking(&symbol_clone, range, &timeframe_clone)
            }).await??;
            
            if result.is_empty() {
                // Real-time data source returned empty. This could mean:
                // 1. RealtimeIngesterService hasn't started downloading data yet
                // 2. The data in MmapStore is from an earlier time period
                // 3. The requested time range is in the future
                // 
                // Since the requested range is >= safe_cutoff, we should NOT fallback to historical data
                // (historical data doesn't exist for future dates). Instead, we return empty and let
                // the UI wait for RealtimeIngesterService to download the data.
                log::warn!(
                    "UnifiedDataService: real-time data source returned 0 K-lines for range {} - {} us. \
                    This likely means RealtimeIngesterService hasn't downloaded data for this time range yet. \
                    The UI should trigger IngestCommand::Subscribe to start data ingestion, or wait for data to be downloaded.",
                    range.start_us,
                    range.end_us
                );
            } else {
                // log::info!("UnifiedDataService: real-time data returned {} K-lines", result.len());
            }
            
            Ok(result)
        } else if range.end_us < safe_cutoff {
            // Pure historical data: download or read from cache
            // log::info!("UnifiedDataService: using historical data source (cache/download)");
            let result = self.batch_layer.fetch_klines(&symbol, range, timeframe).await;
            match &result {
                Ok(_klines) => {}, // log::info!("UnifiedDataService: historical data returned {} K-lines", klines.len()),
                Err(e) => log::warn!("UnifiedDataService: historical data fetch failed: {:?}", e),
            }
            result
        } else {
            // Cross-boundary: fetch separately and merge
            let historical_future = self.batch_layer.fetch_klines(
                &symbol,
                TimeRange {
                    start_us: range.start_us,
                    end_us: safe_cutoff,
                },
                timeframe,
            );
            let symbol_clone = symbol.clone();
            let timeframe_clone = timeframe.to_string();
            let realtime_future = tokio::task::spawn_blocking({
                let service = self.speed_layer.clone();
                move || service.fetch_klines_blocking(&symbol_clone, TimeRange {
                    start_us: safe_cutoff,
                    end_us: range.end_us,
                }, &timeframe_clone)
            });

            // Await both futures concurrently
            // log::info!("UnifiedDataService: cross-boundary query, fetching from both sources");
            let (historical_result, realtime_join_result) = tokio::join!(
                historical_future,
                realtime_future
            );

            // Merge historical and real-time data
            let mut historical = historical_result?;
            let realtime = realtime_join_result??;
            
            // log::info!(
            //     "UnifiedDataService: cross-boundary merge - historical: {} K-lines, real-time: {} K-lines",
            //     historical.len(),
            //     realtime.len()
            // );

            // If real-time data is empty, we still have historical data
            // If both are empty, we'll return an empty vector (which is correct)
            historical.extend(realtime);
            historical.sort_by_key(|k| k.open_time_us);
            historical.dedup_by_key(|k| k.open_time_us);
            
            // log::info!(
            //     "UnifiedDataService: merged result: {} K-lines",
            //     historical.len()
            // );

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
            let symbol_clone = symbol.clone();
            let result = tokio::task::spawn_blocking({
                let service = self.speed_layer.clone();
                move || service.fetch_ticks_blocking(&symbol_clone, range)
            }).await.map_err(|e| DataError::InternalTask(e.to_string()))??;
            Ok(result)
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
            let symbol_clone = symbol.clone();
            let realtime_future = tokio::task::spawn_blocking({
                let service = self.speed_layer.clone();
                move || service.fetch_ticks_blocking(&symbol_clone, TimeRange {
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
    let midnight_utc = midnight.and_utc();
    let cutoff = midnight_utc.timestamp_micros() as u64;

    // Calculate hours since midnight (in microseconds, then convert to hours)
    // timestamp_micros() returns i64, convert to u64 safely
    let now_micros = now.timestamp_micros() as u64;
    let hours_since_midnight = if now_micros >= cutoff {
        (now_micros - cutoff) / 3_600_000_000
    } else {
        // This shouldn't happen (now should always be >= midnight), but handle it gracefully
        24 // Force use of yesterday's boundary
    };
    
    // log::info!(
    //     "calculate_safe_historical_cutoff: now={:?} ({} us), today={:?}, midnight={:?} ({} us), hours_since_midnight={}",
    //     now,
    //     now_micros,
    //     today,
    //     midnight_utc,
    //     cutoff,
    //     hours_since_midnight
    // );
    
    if hours_since_midnight < 6 {
        // Use previous day's boundary (historical data may not be published yet)
        let yesterday = today.pred_opt().unwrap_or(today);
        let yesterday_midnight = yesterday.and_hms_opt(0, 0, 0).unwrap();
        let yesterday_cutoff = yesterday_midnight.and_utc().timestamp_micros() as u64;
        // log::info!(
        //     "calculate_safe_historical_cutoff: using yesterday's boundary: {} us ({:?})",
        //     yesterday_cutoff,
        //     yesterday_midnight.and_utc()
        // );
        yesterday_cutoff
    } else {
        // Use today's boundary (historical data should be published)
        // log::info!(
        //     "calculate_safe_historical_cutoff: using today's boundary: {} us ({:?})",
        //     cutoff,
        //     midnight_utc
        // );
        cutoff
    }
}

