//! Service for reading historical data from cache.
//!
//! This service is responsible for:
//! - Reading historical K-line and Tick data from Parquet cache files
//! - Automatically triggering downloads if cache misses occur
//! - Filtering data to the requested time range

use std::path::PathBuf;
use std::sync::Arc;
use chrono::{TimeZone, Utc};

use crate::{
    data_error::DataError,
    historical_ingester::HistoricalIngesterService,
    kline::KLine,
    compute::vp::TickDataBuffer,
    TimeRange,
};

/// Service for reading historical data from cache.
#[derive(Clone)]
pub struct HistoricalDataService {
    ingester: Arc<HistoricalIngesterService>,
    cache_dir: PathBuf,
}

impl HistoricalDataService {
    /// Creates a new `HistoricalDataService`.
    pub fn new(ingester: Arc<HistoricalIngesterService>) -> Self {
        let cache_dir = ingester.cache_dir.clone();
        Self {
            ingester,
            cache_dir,
        }
    }

    /// Fetches historical K-line data for a given symbol and time range.
    ///
    /// Automatically triggers download if cache misses occur.
    pub async fn fetch_kline(
        &self,
        symbol: &str,
        range: TimeRange,
        timeframe: &str, // e.g., "1m", "5m", "1h"
    ) -> Result<Vec<KLine>, DataError> {
        // 1. Calculate date range for the requested time range
        let dates = calculate_date_range(range);

        let mut all_klines = Vec::new();

        for date in dates {
            // 2. Check cache
            let cache_key = format!("{}_{}_{}.parquet", symbol, date, timeframe);
            let cache_path = self.cache_dir.join(cache_key);

            let klines = if cache_path.exists() {
                // 3. Cache hit: load from cache
                match self.ingester.load_klines_from_cache(&cache_path).await {
                    Ok(cached_klines) => cached_klines,
                    Err(e) => {
                        log::warn!("Cache file corrupted, will re-download: {}", e);
                        // Cache corrupted, trigger download
                        self.ingester.download_and_cache_kline(symbol, &date, timeframe).await?
                    }
                }
            } else {
                // 4. Cache miss: trigger download
                self.ingester.download_and_cache_kline(symbol, &date, timeframe).await?
            };

            // 5. Filter to visible range (only keep needed data)
            let filtered: Vec<KLine> = klines
                .into_iter()
                .filter(|k| k.open_time_us >= range.start_us && k.open_time_us < range.end_us)
                .collect();

            all_klines.extend(filtered);
        }

        Ok(all_klines)
    }

    /// Fetches historical Tick data for a given symbol and time range.
    ///
    /// Automatically triggers download if cache misses occur.
    pub async fn fetch_ticks(
        &self,
        symbol: &str,
        range: TimeRange,
    ) -> Result<TickDataBuffer, DataError> {
        // 1. Calculate date range for the requested time range
        let dates = calculate_date_range(range);

        let mut all_prices = Vec::new();
        let mut all_volumes = Vec::new();

        for date in dates {
            // 2. Check cache
            let cache_key = format!("{}_{}_ticks.parquet", symbol, date);
            let cache_path = self.cache_dir.join(cache_key);

            let ticks = if cache_path.exists() {
                // 3. Cache hit: load from cache
                match self.ingester.load_ticks_from_cache(&cache_path).await {
                    Ok(cached_ticks) => cached_ticks,
                    Err(e) => {
                        log::warn!("Cache file corrupted, will re-download: {}", e);
                        // Cache corrupted, trigger download
                        self.ingester.download_and_cache_ticks(symbol, &date).await?
                    }
                }
            } else {
                // 4. Cache miss: trigger download
                self.ingester.download_and_cache_ticks(symbol, &date).await?
            };

            // 5. Filter to visible range (only keep needed data)
            // Note: Tick data doesn't have timestamps in the buffer, so we can't filter here
            // This would need to be handled at a higher level if timestamps are needed
            all_prices.extend(ticks.prices);
            all_volumes.extend(ticks.volumes);
        }

        Ok(TickDataBuffer {
            prices: all_prices,
            volumes: all_volumes,
        })
    }
}

/// Calculates the date range (as strings) for a given time range.
fn calculate_date_range(range: TimeRange) -> Vec<String> {
    let start_secs = (range.start_us / 1_000_000) as i64;
    let end_secs = (range.end_us / 1_000_000) as i64;
    
    let start_dt = Utc.timestamp_opt(start_secs, 0).unwrap();
    let end_dt = Utc.timestamp_opt(end_secs, 0).unwrap();
    
    let start_date = start_dt.date_naive();
    let end_date = end_dt.date_naive();

    let mut dates = Vec::new();
    let mut current_date = start_date;
    
    while current_date <= end_date {
        dates.push(current_date.format("%Y-%m-%d").to_string());
        if let Some(next) = current_date.succ_opt() {
            current_date = next;
        } else {
            break;
        }
    }

    dates
}

