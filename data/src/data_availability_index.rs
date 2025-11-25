//! Data availability index for tracking the status of historical data.
//!
//! This module provides a lightweight index to track the availability status
//! of historical data for each symbol and date, avoiding redundant download
//! attempts and improving system efficiency.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use serde::{Deserialize, Serialize};

use crate::{data_error::DataError, data_path, event_bus::DataAvailability};

/// Data availability status for a specific symbol and date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataAvailabilityStatus {
    /// Data is available in cache.
    Available,
    /// Data is currently being downloaded.
    Downloading,
    /// Data is unavailable (404, or beyond publication delay).
    Unavailable,
    /// Partial data (cache corrupted or incomplete).
    Partial,
    /// Unknown status (first query).
    Unknown,
}

impl From<DataAvailability> for DataAvailabilityStatus {
    fn from(status: DataAvailability) -> Self {
        match status {
            DataAvailability::Available => DataAvailabilityStatus::Available,
            DataAvailability::Downloading => DataAvailabilityStatus::Downloading,
            DataAvailability::Unavailable => DataAvailabilityStatus::Unavailable,
            DataAvailability::Partial => DataAvailabilityStatus::Partial,
            DataAvailability::Unknown => DataAvailabilityStatus::Unknown,
        }
    }
}

impl From<DataAvailabilityStatus> for DataAvailability {
    fn from(status: DataAvailabilityStatus) -> Self {
        match status {
            DataAvailabilityStatus::Available => DataAvailability::Available,
            DataAvailabilityStatus::Downloading => DataAvailability::Downloading,
            DataAvailabilityStatus::Unavailable => DataAvailability::Unavailable,
            DataAvailabilityStatus::Partial => DataAvailability::Partial,
            DataAvailabilityStatus::Unknown => DataAvailability::Unknown,
        }
    }
}

/// Entry in the availability index.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AvailabilityEntry {
    status: DataAvailabilityStatus,
    last_checked: Option<u64>, // Unix timestamp in seconds
}

/// Data availability index.
///
/// This index tracks the availability status of historical data for each
/// symbol and date, allowing the system to avoid redundant download attempts
/// and make informed decisions about data fetching.
///
/// The index is persisted to disk as a JSON file and automatically loaded
/// on startup and saved periodically.
pub struct DataAvailabilityIndex {
    // symbol -> date -> (status, last_checked)
    index: Arc<RwLock<HashMap<String, HashMap<String, AvailabilityEntry>>>>,
    persistence_path: PathBuf,
    last_saved: Arc<RwLock<Instant>>,
    save_interval: std::time::Duration,
}

impl DataAvailabilityIndex {
    /// Creates a new `DataAvailabilityIndex`.
    ///
    /// The index will be loaded from disk if a persistence file exists,
    /// otherwise it will start empty.
    pub fn new() -> Self {
        Self::with_persistence_path(None)
    }

    /// Creates a new `DataAvailabilityIndex` with a custom persistence path.
    pub fn with_persistence_path(persistence_path: Option<PathBuf>) -> Self {
        let persistence_path = persistence_path
            .unwrap_or_else(|| data_path(Some("data_availability_index.json")));
        
        let index = Arc::new(RwLock::new(HashMap::new()));
        
        // Try to load from disk
        if let Ok(loaded) = Self::load_from_disk(&persistence_path) {
            *index.blocking_write() = loaded;
        }
        
        Self {
            index,
            persistence_path,
            last_saved: Arc::new(RwLock::new(Instant::now())),
            save_interval: std::time::Duration::from_secs(60), // Save every minute
        }
    }

    /// Checks the availability status for a specific symbol and date.
    ///
    /// Returns `DataAvailability::Unknown` if the entry doesn't exist.
    pub async fn check_availability(
        &self,
        symbol: &str,
        date: &str,
    ) -> DataAvailability {
        let index = self.index.read().await;
        index
            .get(symbol)
            .and_then(|dates| dates.get(date))
            .map(|entry| entry.status.into())
            .unwrap_or(DataAvailability::Unknown)
    }

    /// Updates the availability status for a specific symbol and date.
    pub async fn update_availability(
        &self,
        symbol: &str,
        date: &str,
        status: DataAvailability,
    ) {
        let mut index = self.index.write().await;
        let symbol_map = index.entry(symbol.to_string()).or_insert_with(HashMap::new);
        
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        symbol_map.insert(
            date.to_string(),
            AvailabilityEntry {
                status: status.into(),
                last_checked: Some(now),
            },
        );
        
        drop(index);
        
        // Auto-save if enough time has passed
        self.maybe_save().await;
    }

    /// Checks availability for a range of dates for a specific symbol.
    ///
    /// Returns a vector of (date, availability) pairs.
    pub async fn check_date_range(
        &self,
        symbol: &str,
        dates: &[String],
    ) -> Vec<(String, DataAvailability)> {
        let index = self.index.read().await;
        dates
            .iter()
            .map(|date| {
                let status = index
                    .get(symbol)
                    .and_then(|dates| dates.get(date))
                    .map(|entry| entry.status.into())
                    .unwrap_or(DataAvailability::Unknown);
                (date.clone(), status)
            })
            .collect()
    }

    /// Gets all symbols in the index.
    pub async fn get_all_symbols(&self) -> Vec<String> {
        let index = self.index.read().await;
        index.keys().cloned().collect()
    }

    /// Gets all dates for a specific symbol.
    pub async fn get_dates_for_symbol(&self, symbol: &str) -> Vec<String> {
        let index = self.index.read().await;
        index
            .get(symbol)
            .map(|dates| dates.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Gets the last checked timestamp for a specific symbol and date.
    pub async fn get_last_checked(&self, symbol: &str, date: &str) -> Option<Instant> {
        let index = self.index.read().await;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        
        index
            .get(symbol)
            .and_then(|dates| dates.get(date))
            .and_then(|entry| entry.last_checked)
            .map(|entry_secs| {
                let elapsed = now_secs.saturating_sub(entry_secs);
                Instant::now().checked_sub(std::time::Duration::from_secs(elapsed))
                    .unwrap_or(Instant::now())
            })
    }

    /// Saves the index to disk.
    pub async fn save_to_disk(&self) -> Result<(), DataError> {
        let index = self.index.read().await;
        
        // Create parent directory if it doesn't exist
        if let Some(parent) = self.persistence_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        let serialized = serde_json::to_string_pretty(&*index)?;
        std::fs::write(&self.persistence_path, serialized)?;
        
        *self.last_saved.write().await = Instant::now();
        
        Ok(())
    }

    /// Loads the index from disk.
    fn load_from_disk(path: &PathBuf) -> Result<HashMap<String, HashMap<String, AvailabilityEntry>>, DataError> {
        if !path.exists() {
            return Ok(HashMap::new());
        }
        
        let contents = std::fs::read_to_string(path)?;
        let index: HashMap<String, HashMap<String, AvailabilityEntry>> = serde_json::from_str(&contents)?;
        
        Ok(index)
    }

    /// Saves to disk if enough time has passed since the last save.
    async fn maybe_save(&self) {
        let last_saved = *self.last_saved.read().await;
        if last_saved.elapsed() >= self.save_interval {
            if let Err(e) = self.save_to_disk().await {
                log::warn!("Failed to save data availability index: {}", e);
            }
        }
    }

    /// Forces a save to disk immediately.
    pub async fn force_save(&self) -> Result<(), DataError> {
        self.save_to_disk().await
    }
}

impl Default for DataAvailabilityIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_check_availability_unknown() {
        let index = DataAvailabilityIndex::new();
        let status = index.check_availability("BTCUSDT", "2025-11-25").await;
        assert_eq!(status, DataAvailability::Unknown);
    }

    #[tokio::test]
    async fn test_update_and_check_availability() {
        let index = DataAvailabilityIndex::new();
        
        index.update_availability("BTCUSDT", "2025-11-25", DataAvailability::Available).await;
        let status = index.check_availability("BTCUSDT", "2025-11-25").await;
        assert_eq!(status, DataAvailability::Available);
        
        index.update_availability("BTCUSDT", "2025-11-25", DataAvailability::Downloading).await;
        let status = index.check_availability("BTCUSDT", "2025-11-25").await;
        assert_eq!(status, DataAvailability::Downloading);
    }

    #[tokio::test]
    async fn test_check_date_range() {
        let index = DataAvailabilityIndex::new();
        
        index.update_availability("BTCUSDT", "2025-11-25", DataAvailability::Available).await;
        index.update_availability("BTCUSDT", "2025-11-24", DataAvailability::Unavailable).await;
        
        let dates = vec!["2025-11-25".to_string(), "2025-11-24".to_string(), "2025-11-23".to_string()];
        let results = index.check_date_range("BTCUSDT", &dates).await;
        
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].1, DataAvailability::Available);
        assert_eq!(results[1].1, DataAvailability::Unavailable);
        assert_eq!(results[2].1, DataAvailability::Unknown);
    }

    #[tokio::test]
    async fn test_persistence() {
        use std::env;
        let temp_file = env::temp_dir().join("test_data_availability_index.json");
        
        // Create index and update
        let index1 = DataAvailabilityIndex::with_persistence_path(Some(temp_file.clone()));
        index1.update_availability("BTCUSDT", "2025-11-25", DataAvailability::Available).await;
        index1.force_save().await.unwrap();
        
        // Create new index and load
        let index2 = DataAvailabilityIndex::with_persistence_path(Some(temp_file));
        let status = index2.check_availability("BTCUSDT", "2025-11-25").await;
        assert_eq!(status, DataAvailability::Available);
    }
}

