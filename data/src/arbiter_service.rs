//! The core data arbitration service.
//!
//! This module contains the main logic for fetching K-line data from multiple
//! sources concurrently, resolving conflicts, and providing a single, unified
//! stream of data to the application.

use chrono::{Duration, Utc};
use tokio::task::JoinHandle;

use crate::{
    arbiter_error::ArbiterError,
    external_adapter::ExternalAdapter,
    io_service::{IoService, TimeRange},
    kline::KLine,
};

/// The main service for arbitrating between different data sources.
///
/// It holds instances of the services responsible for I/O and external API calls.
pub struct ArbiterService {
    io_service: IoService,
    external_adapter: ExternalAdapter,
}

impl ArbiterService {
    /// Creates a new `ArbiterService`.
    pub fn new(io_service: IoService, external_adapter: ExternalAdapter) -> Self {
        Self {
            io_service,
            external_adapter,
        }
    }

    /// Returns a reference to the IoService for direct access to raw data.
    pub fn io_service(&self) -> &IoService {
        &self.io_service
    }

    /// Fetches K-line data for a given symbol and time range from all available
    /// sources concurrently and merges the results.
    ///
    /// This is the primary entry point for data fetching in the application. It
    /// handles I/O isolation by spawning tasks on the appropriate Tokio runtimes.
    pub async fn arbitrate_kline_data(
        &self,
        symbol: String,
        range: TimeRange,
    ) -> Result<Vec<KLine>, ArbiterError> {
        // Clone the services to move them into the async tasks.
        let io_service = self.io_service.clone();
        let external_adapter = self.external_adapter.clone();

        // 1. Spawn the I/O-bound task on a blocking thread.
        let live_data_handle: JoinHandle<Result<Vec<KLine>, ArbiterError>> =
            tokio::task::spawn_blocking(move || io_service.fetch_live_kline_blocking(range));

        // 2. Spawn the network-bound task on the main async runtime.
        let historical_data_handle: JoinHandle<Result<Vec<KLine>, ArbiterError>> =
            tokio::spawn(async move { external_adapter.fetch_historical_kline(&symbol, range).await });

        // 3. Concurrently await both results.
        // `try_join` will poll both futures and return immediately if either fails.
        let (live_result, historical_result) =
            tokio::try_join!(live_data_handle, historical_data_handle)?;

        // The `?` operator after `try_join!` handles the `JoinError`. Now we handle the
        // `ArbiterError` from inside the tasks' `Result`s.
        let live_data = live_result?;
        let historical_data = historical_result?;

        // 5. Merge the data and resolve any conflicts.
        Ok(merge_and_resolve(live_data, historical_data, range))
    }
}

/// Merges K-line data from internal (live) and external (historical) sources.
///
/// This function enforces the data authority rules, where live data is trusted
/// over historical data within the "Live Window".
fn merge_and_resolve(
    live_data: Vec<KLine>,
    mut historical_data: Vec<KLine>,
    _range: TimeRange,
) -> Vec<KLine> {
    // 1. Define the "Live Window" as the last 12 hours from the current time.
    let now_ns = Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64;
    let live_window_start_ns = now_ns.saturating_sub(Duration::hours(12).num_nanoseconds().unwrap_or(0) as u64);

    // 2. Filter historical data, removing any data that falls within the live window.
    // Live data is considered the source of truth for the recent past.
    historical_data.retain(|k| k.open_time_ns < live_window_start_ns);

    // 3. Combine the authoritative live data with the filtered historical data.
    let mut combined = live_data;
    combined.extend(historical_data);

    // 4. Sort the combined data by timestamp to ensure chronological order.
    // This is crucial as the two sources are fetched concurrently.
    combined.sort_by_key(|k| k.open_time_ns);

    // 5. Remove any consecutive duplicates that might arise at the merge boundary.
    combined.dedup_by_key(|k| k.open_time_ns);

    combined
}
