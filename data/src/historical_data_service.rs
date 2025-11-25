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
    data_availability_index::DataAvailabilityIndex,
    data_error::DataError,
    event_bus::{DataType, DownloadPriority},
    historical_download_coordinator::{DownloadTask, HistoricalDownloadCoordinator},
    historical_download_executor::HistoricalDownloadExecutor,
    kline::KLine,
    compute::vp::TickDataBuffer,
    TimeRange,
};

/// Service for reading historical data from cache.
#[derive(Clone)]
pub struct HistoricalDataService {
    executor: Arc<HistoricalDownloadExecutor>,
    cache_dir: PathBuf,
    availability_index: Arc<DataAvailabilityIndex>,
    download_coordinator: Arc<HistoricalDownloadCoordinator>,
}

impl HistoricalDataService {
    /// Creates a new `HistoricalDataService`.
    pub fn new(
        executor: Arc<HistoricalDownloadExecutor>,
        availability_index: Arc<DataAvailabilityIndex>,
        download_coordinator: Arc<HistoricalDownloadCoordinator>,
    ) -> Self {
        let cache_dir = executor.cache_dir.clone();
        Self {
            executor,
            cache_dir,
            availability_index,
            download_coordinator,
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
        
        // 2. Check data availability index and submit download tasks for missing data
        let availability_results = self.availability_index.check_date_range(symbol, &dates).await;
        let missing_dates: Vec<String> = availability_results
            .into_iter()
            .filter(|(_, status)| {
                matches!(status, crate::event_bus::DataAvailability::Unknown | crate::event_bus::DataAvailability::Unavailable)
            })
            .map(|(date, _)| date)
            .collect();
        
        // 3. Submit download tasks (non-blocking, UserRequest priority)
        if !missing_dates.is_empty() {
            log::info!("[HistoricalDataService] Submitting {} download tasks for {} (timeframe: {})", 
                missing_dates.len(), symbol, timeframe);
        }
        for date in &missing_dates {
            log::info!("[HistoricalDataService] Submitting download task: {} {} {}", symbol, date, timeframe);
            self.download_coordinator.submit_task(DownloadTask::new(
                symbol.to_string(),
                date.clone(),
                DataType::Kline { timeframe: Some(timeframe.to_string()) },
                Some(timeframe.to_string()),
                DownloadPriority::UserRequest,
            )).await;
        }

        let mut all_klines = Vec::new();

        // 4. Load available data (don't wait for downloads to complete)
        for date in dates {
            let cache_key = format!("{}_{}_{}.parquet", symbol, date, timeframe);
            let cache_path = self.cache_dir.join(cache_key);

            let klines = if cache_path.exists() {
                // Cache hit: load from cache
                match self.executor.load_klines_from_cache(&cache_path).await {
                    Ok(cached_klines) => cached_klines,
                    Err(e) => {
                        log::warn!("Cache file corrupted for {}/{}/{}: {}. Will be re-downloaded.", symbol, timeframe, date, e);
                        // Mark as Partial and trigger re-download
                        self.availability_index
                            .update_availability(symbol, &date, crate::event_bus::DataAvailability::Partial)
                            .await;
                        self.download_coordinator.submit_task(DownloadTask::new(
                            symbol.to_string(),
                            date.clone(),
                            DataType::Kline { timeframe: Some(timeframe.to_string()) },
                            Some(timeframe.to_string()),
                            DownloadPriority::UserRequest,
                        )).await;
                        Vec::new()
                    }
                }
            } else {
                // Cache miss: data will be downloaded by download service
                // Return empty for now, data will be available after download completes
                Vec::new()
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
        
        // 2. Check data availability index and submit download tasks for missing data
        let availability_results = self.availability_index.check_date_range(symbol, &dates).await;
        let missing_dates: Vec<String> = availability_results
            .into_iter()
            .filter(|(_, status)| {
                matches!(status, crate::event_bus::DataAvailability::Unknown | crate::event_bus::DataAvailability::Unavailable)
            })
            .map(|(date, _)| date)
            .collect();
        
        // 3. Submit download tasks (non-blocking, UserRequest priority)
        for date in &missing_dates {
            self.download_coordinator.submit_task(DownloadTask::new(
                symbol.to_string(),
                date.clone(),
                DataType::Tick,
                None,
                DownloadPriority::UserRequest,
            )).await;
        }

        let mut all_prices = Vec::new();
        let mut all_volumes = Vec::new();
        let mut merged_time_range: Option<(u64, u64)> = None;

        // 4. Load available data (don't wait for downloads to complete)
        for date in &dates {
            let cache_key = format!("{}_{}_ticks.parquet", symbol, date);
            let cache_path = self.cache_dir.join(cache_key);

            let ticks = if cache_path.exists() {
                // Cache hit: load from cache
                match self.executor.load_ticks_from_cache(&cache_path).await {
                    Ok(cached_ticks) => {
                        log::debug!(
                            "[HistoricalDataService] Loaded {} ticks for {}/{} (time_range: {:?})",
                            cached_ticks.prices.len(),
                            symbol,
                            date,
                            cached_ticks.time_range
                        );
                        cached_ticks
                    }
                    Err(e) => {
                        log::warn!("Cache file corrupted for {}/{}: {}. Will be re-downloaded.", symbol, date, e);
                        // Mark as Partial and trigger re-download
                        self.availability_index
                            .update_availability(symbol, date, crate::event_bus::DataAvailability::Partial)
                            .await;
                        self.download_coordinator.submit_task(DownloadTask::new(
                            symbol.to_string(),
                            date.clone(),
                            DataType::Tick,
                            None,
                            DownloadPriority::UserRequest,
                        )).await;
                        TickDataBuffer {
                            prices: Vec::new(),
                            volumes: Vec::new(),
                            time_range: None,
                        }
                    }
                }
            } else {
                log::debug!("[HistoricalDataService] Cache miss for {}/{}", symbol, date);
                // Cache miss: data will be downloaded by download service
                // Return empty for now, data will be available after download completes
                TickDataBuffer {
                    prices: Vec::new(),
                    volumes: Vec::new(),
                    time_range: None,
                }
            };

            // 5. Merge data and time ranges
            let tick_count = ticks.prices.len();
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
            } else if tick_count > 0 {
                // Data exists but no time_range (old format, e.g., 23号)
                log::debug!("[HistoricalDataService] {}/{} has {} ticks but no time_range (old format)", symbol, date, tick_count);
            }
        }
        
        log::info!(
            "[HistoricalDataService] Merged {} ticks from {} dates, time_range: {:?}",
            all_prices.len(),
            dates.len(),
            merged_time_range
        );

        // If we have data but time_range is incomplete (some dates missing),
        // we should still compute the expected time range based on requested dates
        // to help with data completeness validation
        // However, if we have no data at all, we can't infer a time range
        if merged_time_range.is_none() && !all_prices.is_empty() {
            // This shouldn't happen if data has timestamps, but handle gracefully
            log::warn!("Historical tick data has no time_range but contains {} ticks. This may indicate old cache format.", all_prices.len());
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

