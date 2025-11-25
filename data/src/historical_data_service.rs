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
    pub async fn fetch_klines(
        &self,
        symbol: &str,
        range: TimeRange,
        timeframe: &str, // e.g., "1m", "5m", "1h"
    ) -> Result<Vec<KLine>, DataError> {
        // 1. Calculate date range for the requested time range
        // Note: This should only include dates BEFORE safe_cutoff (today's midnight)
        // The UnifiedDataService ensures range.end_us <= safe_cutoff, but we add an extra check
        let safe_cutoff = calculate_safe_historical_cutoff();
        let effective_end = range.end_us.min(safe_cutoff);
        let effective_range = TimeRange {
            start_us: range.start_us,
            end_us: effective_end,
        };
        let mut dates = calculate_date_range(effective_range);
        
        // Filter out dates that are >= safe_cutoff date
        // This ensures we never try to download today's historical data
        let cutoff_date = {
            let cutoff_secs = (safe_cutoff / 1_000_000) as i64;
            let cutoff_dt = Utc.timestamp_opt(cutoff_secs, 0).unwrap();
            cutoff_dt.date_naive()
        };
        dates.retain(|date_str| {
            if let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                date < cutoff_date
            } else {
                false
            }
        });

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
                        // If download fails (e.g., 404 for today's data), continue with empty data
                        match self.ingester.download_and_cache_kline(symbol, &date, timeframe).await {
                            Ok(downloaded_klines) => downloaded_klines,
                            Err(e) => {
                                log::warn!("Failed to download K-lines for {}/{}/{}: {}. Continuing with empty data.", symbol, timeframe, date, e);
                                Vec::new()
                            }
                        }
                    }
                }
            } else {
                // 4. Cache miss: trigger download
                // If download fails (e.g., 404 for today's data), continue with empty data
                match self.ingester.download_and_cache_kline(symbol, &date, timeframe).await {
                    Ok(downloaded_klines) => downloaded_klines,
                    Err(e) => {
                        log::warn!("Failed to download K-lines for {}/{}/{}: {}. Continuing with empty data.", symbol, timeframe, date, e);
                        Vec::new()
                    }
                }
            };

            // 5. Filter to visible range (only keep needed data)
            let filtered: Vec<KLine> = klines
                .into_iter()
                .filter(|k| k.open_time_us >= effective_range.start_us && k.open_time_us < effective_range.end_us)
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
        // Note: This should only include dates BEFORE safe_cutoff (today's midnight)
        // The UnifiedDataService ensures range.end_us <= safe_cutoff, but we add an extra check
        let safe_cutoff = calculate_safe_historical_cutoff();
        let effective_end = range.end_us.min(safe_cutoff);
        let effective_range = TimeRange {
            start_us: range.start_us,
            end_us: effective_end,
        };
        let mut dates = calculate_date_range(effective_range);
        
        // Filter out dates that are >= safe_cutoff date
        // This ensures we never try to download today's historical data
        let cutoff_date = {
            let cutoff_secs = (safe_cutoff / 1_000_000) as i64;
            let cutoff_dt = Utc.timestamp_opt(cutoff_secs, 0).unwrap();
            cutoff_dt.date_naive()
        };
        dates.retain(|date_str| {
            if let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                date < cutoff_date
            } else {
                false
            }
        });
        

        let mut all_prices = Vec::new();
        let mut all_volumes = Vec::new();
        let mut merged_time_range: Option<(u64, u64)> = None;

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
                        // If download fails (e.g., 404 for today's data), continue with empty data
                        match self.ingester.download_and_cache_ticks(symbol, &date).await {
                            Ok(downloaded_ticks) => downloaded_ticks,
                            Err(e) => {
                                log::warn!("Failed to download ticks for {}/{}: {}. Continuing with empty data.", symbol, date, e);
                                TickDataBuffer {
                                    prices: Vec::new(),
                                    volumes: Vec::new(),
                                    time_range: None,
                                }
                            }
                        }
                    }
                }
            } else {
                // 4. Cache miss: trigger download
                // If download fails (e.g., 404 for today's data), continue with empty data
                match self.ingester.download_and_cache_ticks(symbol, &date).await {
                    Ok(downloaded_ticks) => downloaded_ticks,
                    Err(e) => {
                        log::warn!("Failed to download ticks for {}/{}: {}. Continuing with empty data.", symbol, date, e);
                        TickDataBuffer {
                            prices: Vec::new(),
                            volumes: Vec::new(),
                            time_range: None,
                        }
                    }
                }
            };

            // 5. Merge data and time ranges
            all_prices.extend(ticks.prices);
            all_volumes.extend(ticks.volumes);
            
            // Merge time ranges
            if let Some((start, end)) = ticks.time_range {
                merged_time_range = Some(match merged_time_range {
                    Some((merged_start, merged_end)) => {
                        (merged_start.min(start), merged_end.max(end))
                    }
                    None => (start, end),
                });
            }
        }

        // Historical data now tracks time_range from timestamps
        Ok(TickDataBuffer {
            prices: all_prices,
            volumes: all_volumes,
            time_range: merged_time_range,
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

/// Calculates the safe historical data cutoff time.
///
/// This function determines the time boundary between historical and real-time data.
/// Historical data is considered "safe" (i.e., fully published and available) up to
/// this cutoff time. Data after this time should be fetched from real-time sources.
///
/// The cutoff is typically set to today's midnight (UTC), but can be adjusted to
/// yesterday's midnight if it's too early in the day (to account for publication delays).
fn calculate_safe_historical_cutoff() -> u64 {
    let now = Utc::now();
    let today = now.date_naive();
    let midnight = today.and_hms_opt(0, 0, 0).unwrap();
    let midnight_utc = midnight.and_utc();
    let cutoff = midnight_utc.timestamp_micros() as u64;
    
    let now_micros = now.timestamp_micros() as u64;
    let hours_since_midnight = if now_micros >= cutoff {
        (now_micros - cutoff) / 3_600_000_000
    } else {
        // This shouldn't happen (now should always be >= midnight), but handle it gracefully
        24 // Force use of yesterday's boundary
    };
    
    
    if hours_since_midnight < 6 {
        // Use previous day's boundary (historical data may not be published yet)
        let yesterday = today.pred_opt().unwrap_or(today);
        let yesterday_midnight = yesterday.and_hms_opt(0, 0, 0).unwrap();
        let yesterday_cutoff = yesterday_midnight.and_utc().timestamp_micros() as u64;
        yesterday_cutoff
    } else {
        // Use today's boundary (historical data should be published)
        cutoff
    }
}

