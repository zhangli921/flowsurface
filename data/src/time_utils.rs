//! Time-related utility functions for data services.

use chrono::{TimeZone, Utc};
use crate::TimeRange;

/// Calculates the date range (as strings) for a given time range.
pub fn calculate_date_range(range: TimeRange) -> Vec<String> {
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
pub fn calculate_safe_historical_cutoff() -> u64 {
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

/// Filters out dates that are >= safe_cutoff date.
///
/// This ensures we never try to download today's historical data.
pub fn filter_historical_dates(dates: &mut Vec<String>, safe_cutoff: u64) {
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
}

/// Checks if a timestamp (in microseconds) is within today's date.
///
/// This is used to determine if data should be fetched from real-time sources
/// (Mmap) or historical sources (Parquet cache). Only data from today should
/// be fetched from real-time sources.
///
/// # Arguments
/// - `timestamp_us`: Timestamp in microseconds
///
/// # Returns
/// - `true` if the timestamp is today
/// - `false` if the timestamp is before today
pub fn is_today(timestamp_us: u64) -> bool {
    let now = Utc::now();
    let today = now.date_naive();
    let today_midnight = today.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let today_midnight_us = today_midnight.timestamp_micros() as u64;
    
    let tomorrow_midnight = today.succ_opt()
        .unwrap_or(today)
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let tomorrow_midnight_us = tomorrow_midnight.timestamp_micros() as u64;
    
    timestamp_us >= today_midnight_us && timestamp_us < tomorrow_midnight_us
}

