//! Defines the unified error type for the data service layer.
//!
//! This error type is used across all data services including:
//! - UnifiedDataService
//! - RealtimeDataService
//! - HistoricalDataService
//! - HistoricalDownloadExecutor
//! - VP computation services

use thiserror::Error;

/// A comprehensive error enum for the data service layer.
///
/// This type consolidates errors from various sources, including:
/// - Mmap storage access (`storage::StoreError`).
/// - Asynchronous task execution (`tokio::task::JoinError`).
/// - Network requests (`reqwest::Error`).
/// - Data serialization/deserialization (e.g., Parquet, JSON).
/// - Standard I/O operations.
/// - ZIP archive operations.
/// - CSV parsing.
#[derive(Debug, Error)]
pub enum DataError {
    #[error("Mmap storage error: {0}")]
    Store(#[from] storage::StoreError),

    #[error("A blocking task failed to execute: {0}")]
    InternalTask(String),

    #[error("Network request failed: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Parquet processing error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    #[error("Arrow processing error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("JSON serialization/deserialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Standard I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid input or configuration: {0}")]
    InvalidInput(String),

    #[error("ZIP archive error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("CSV parsing error: {0}")]
    Csv(#[from] csv::Error),

    #[error("Exchange adapter error: {0}")]
    Adapter(String),
}

// Manual implementation of From<tokio::task::JoinError> to convert it into a concrete string.
// This is necessary because JoinError is not `Send` or `Sync` in all cases if the panic
// payload is not, so we convert it to a string immediately.
impl From<tokio::task::JoinError> for DataError {
    fn from(err: tokio::task::JoinError) -> Self {
        DataError::InternalTask(err.to_string())
    }
}

