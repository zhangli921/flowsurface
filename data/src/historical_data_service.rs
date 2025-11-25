//! Service for reading historical data from cache.
//!
//! This service is responsible for:
//! - Reading historical K-line and Tick data from Parquet cache files
//! - Automatically triggering downloads if cache misses occur
//! - Filtering data to the requested time range

use std::path::PathBuf;
use std::sync::Arc;

use crate::{
    data_availability_index::DataAvailabilityIndex,
    data_error::DataError,
    event_bus::{DataAvailability, DataType, DownloadPriority},
    historical_download_coordinator::{DownloadTask, HistoricalDownloadCoordinator},
    historical_download_executor::HistoricalDownloadExecutor,
    kline::KLine,
    compute::vp::TickDataBuffer,
    realtime_ingester::normalize_binance_symbol,
    time_utils::{calculate_date_range, calculate_safe_historical_cutoff, filter_historical_dates},
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
        // Normalize symbol to ensure consistency with cache file names
        let normalized_symbol = normalize_binance_symbol(symbol);
        
        // 1. Calculate date range for the requested time range
        let safe_cutoff = calculate_safe_historical_cutoff();
        let effective_end = range.end_us.min(safe_cutoff);
        let effective_range = TimeRange {
            start_us: range.start_us,
            end_us: effective_end,
        };
        let mut dates = calculate_date_range(effective_range);
        filter_historical_dates(&mut dates, safe_cutoff);
        
        // 2. Check data availability and submit download tasks for missing data
        self.submit_missing_download_tasks(&normalized_symbol, &dates, |date| {
            DownloadTask::new(
                normalized_symbol.clone(),
                date.clone(),
                DataType::Kline { timeframe: Some(timeframe.to_string()) },
                Some(timeframe.to_string()),
                DownloadPriority::UserRequest,
            )
        }).await;

        let mut all_klines = Vec::new();

        // 3. Load available data (don't wait for downloads to complete)
        for date in dates {
            let cache_key = format!("{}_{}_{}.parquet", normalized_symbol, date, timeframe);
            let cache_path = self.cache_dir.join(cache_key);

            let klines = self.load_cached_klines(&cache_path, &normalized_symbol, &date, timeframe).await;

            // 4. Filter to visible range (only keep needed data)
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
        // Normalize symbol to ensure consistency with cache file names
        let normalized_symbol = normalize_binance_symbol(symbol);
        
        // 1. Calculate date range for the requested time range
        let safe_cutoff = calculate_safe_historical_cutoff();
        let effective_end = range.end_us.min(safe_cutoff);
        let effective_range = TimeRange {
            start_us: range.start_us,
            end_us: effective_end,
        };
        let mut dates = calculate_date_range(effective_range);
        filter_historical_dates(&mut dates, safe_cutoff);
        
        // 2. Check data availability and submit download tasks for missing data
        self.submit_missing_download_tasks(&normalized_symbol, &dates, |date| {
            DownloadTask::new(
                normalized_symbol.clone(),
                date.clone(),
                DataType::Tick,
                None,
                DownloadPriority::UserRequest,
            )
        }).await;

        let mut all_prices = Vec::new();
        let mut all_volumes = Vec::new();
        let mut merged_time_range: Option<(u64, u64)> = None;

        // 3. Load available data (don't wait for downloads to complete)
        for date in &dates {
            let cache_key = format!("{}_{}_ticks.parquet", normalized_symbol, date);
            let cache_path = self.cache_dir.join(cache_key);

            let ticks = self.load_cached_ticks(&cache_path, &normalized_symbol, date).await;

            // 4. Merge data and time ranges
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

    /// Submits download tasks for missing dates.
    async fn submit_missing_download_tasks<F>(&self, symbol: &str, dates: &[String], task_factory: F)
    where
        F: Fn(&String) -> DownloadTask,
    {
        let availability_results = self.availability_index.check_date_range(symbol, dates).await;
        let missing_dates: Vec<String> = availability_results
            .into_iter()
            .filter(|(_, status)| {
                matches!(status, DataAvailability::Unknown | DataAvailability::Unavailable)
            })
            .map(|(date, _)| date)
            .collect();
        
        if !missing_dates.is_empty() {
            log::debug!(
                "[HistoricalDataService] Submitting {} download tasks for {}",
                missing_dates.len(),
                symbol
            );
        }
        
        for date in &missing_dates {
            self.download_coordinator.submit_task(task_factory(date)).await;
        }
    }

    /// Loads cached K-lines, handling errors and triggering re-downloads if needed.
    async fn load_cached_klines(
        &self,
        cache_path: &std::path::Path,
        symbol: &str,
        date: &str,
        timeframe: &str,
    ) -> Vec<KLine> {
        if !cache_path.exists() {
            return Vec::new();
        }

        match self.executor.load_klines_from_cache(cache_path).await {
            Ok(klines) => klines,
            Err(e) => {
                log::warn!(
                    "Cache file corrupted for {}/{}/{}: {}. Will be re-downloaded.",
                    symbol,
                    timeframe,
                    date,
                    e
                );
                self.availability_index
                    .update_availability(symbol, date, DataAvailability::Partial)
                    .await;
                self.download_coordinator
                    .submit_task(DownloadTask::new(
                        symbol.to_string(),
                        date.to_string(),
                        DataType::Kline {
                            timeframe: Some(timeframe.to_string()),
                        },
                        Some(timeframe.to_string()),
                        DownloadPriority::UserRequest,
                    ))
                    .await;
                Vec::new()
            }
        }
    }

    /// Loads cached ticks, handling errors and triggering re-downloads if needed.
    async fn load_cached_ticks(
        &self,
        cache_path: &std::path::Path,
        symbol: &str,
        date: &str,
    ) -> TickDataBuffer {
        if !cache_path.exists() {
            log::debug!("[HistoricalDataService] Cache miss for {}/{}", symbol, date);
            return TickDataBuffer {
                prices: Vec::new(),
                volumes: Vec::new(),
                time_range: None,
            };
        }

        match self.executor.load_ticks_from_cache(cache_path).await {
            Ok(ticks) => {
                log::debug!(
                    "[HistoricalDataService] Loaded {} ticks for {}/{} (time_range: {:?})",
                    ticks.prices.len(),
                    symbol,
                    date,
                    ticks.time_range
                );
                ticks
            }
            Err(e) => {
                log::warn!(
                    "Cache file corrupted for {}/{}: {}. Will be re-downloaded.",
                    symbol,
                    date,
                    e
                );
                self.availability_index
                    .update_availability(symbol, date, DataAvailability::Partial)
                    .await;
                self.download_coordinator
                    .submit_task(DownloadTask::new(
                        symbol.to_string(),
                        date.to_string(),
                        DataType::Tick,
                        None,
                        DownloadPriority::UserRequest,
                    ))
                    .await;
                TickDataBuffer {
                    prices: Vec::new(),
                    volumes: Vec::new(),
                    time_range: None,
                }
            }
        }
    }
}

