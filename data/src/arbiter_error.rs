//! Defines the unified error type for the data arbitration service.

use thiserror::Error;

/// A comprehensive error enum for the K-line data arbitration and I/O services.
///
/// This type consolidates errors from various sources, including:
/// - Mmap storage access (`storage::StoreError`).
/// - Asynchronous task execution (`tokio::task::JoinError`).
/// - Network requests (`reqwest::Error`).
/// - Data serialization/deserialization (e.g., Parquet, JSON).
/// - Standard I/O operations.
#[derive(Debug, Error)]
pub enum ArbiterError {
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
    InvalidInput(&'static str),
}

// Manual implementation of From<tokio::task::JoinError> to convert it into a concrete string.
// This is necessary because JoinError is not `Send` or `Sync` in all cases if the panic
// payload is not, so we convert it to a string immediately.
impl From<tokio::task::JoinError> for ArbiterError {
    fn from(err: tokio::task::JoinError) -> Self {
        ArbiterError::InternalTask(err.to_string())
    }
}
