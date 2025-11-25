//! Unified data service that automatically selects between real-time and historical data sources.
//!
//! This service provides a unified interface for fetching K-line and Tick data,
//! automatically choosing between Speed Layer (real-time) and Batch Layer (historical)
//! based on the requested time range and the dynamic time boundary (safe_cutoff).

use std::sync::Arc;

use crate::{
    data_error::DataError,
    realtime_data_service::{RealtimeDataService, TimeRange},
    historical_data_service::HistoricalDataService,
    kline::KLine,
    compute::vp::TickDataBuffer,
    time_utils::calculate_safe_historical_cutoff,
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
            }
            
            Ok(result)
        } else if range.end_us < safe_cutoff {
            // Pure historical data: download or read from cache
            let result = self.batch_layer.fetch_klines(&symbol, range, timeframe).await;
            if let Err(e) = &result {
                log::warn!("UnifiedDataService: historical data fetch failed: {:?}", e);
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
            let (historical_result, realtime_join_result) = tokio::join!(
                historical_future,
                realtime_future
            );

            // Merge historical and real-time data
            let mut historical = historical_result?;
            let realtime = realtime_join_result??;

            // If real-time data is empty, we still have historical data
            // If both are empty, we'll return an empty vector (which is correct)
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
            // Merge time ranges: combine historical and realtime ranges
            let merged_time_range = match (historical.time_range, realtime.time_range) {
                (Some((hist_start, hist_end)), Some((rt_start, rt_end))) => {
                    // Both have ranges: merge them (historical is earlier, realtime is later)
                    Some((hist_start.min(rt_start), hist_end.max(rt_end)))
                }
                (Some(hist_range), None) => Some(hist_range),
                (None, Some(rt_range)) => Some(rt_range),
                (None, None) => None,
            };

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
                time_range: merged_time_range,
            })
        }
    }
}

