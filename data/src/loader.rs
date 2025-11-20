use anyhow::{Context, Result};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use storage::MmapStore;
use bytes::Bytes;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Calculate a u64 hash for a given key.
fn calculate_hash<T: Hash>(t: &T) -> u64 {
    let mut s = DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
}

/// Loads trade data from a memory-mapped file.
///
/// # Arguments
///
/// * `path` - The path to the `.mmap` data file.
/// * `key` - The key for the data block to load (e.g., "trades/BTCUSDT/2025-11-20").
///
/// # Returns
///
/// A `Result` containing the Arrow `RecordBatch` or an error.
pub fn load_trades_from_mmap(path: &Path, key: &str) -> Result<RecordBatch> {
    // 1. Open the mmap store
    let store = MmapStore::open(path).context("Failed to open MmapStore")?;

    // 2. Look up the index for the desired data key
    let key_hash = calculate_hash(&key);
    let index_entry = store
        .lookup_index(key_hash)
        .map_err(|e| anyhow::anyhow!("Failed to find index for key '{}': {:?}", key, e))?;

    // 3. Get the payload bytes for that index
    let payload_bytes = store.get_payload(index_entry);

    // 4. Deserialize the Parquet payload into an Arrow RecordBatch
    let payload_bytes = Bytes::from(payload_bytes.to_vec());
    let reader = ParquetRecordBatchReaderBuilder::try_new(payload_bytes)?
        .with_batch_size(8192)
        .build()?;
    
    // For this example, we assume the parquet file contains a single record batch.
    // A more robust solution would handle multiple batches.
    let record_batch = reader
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No RecordBatches found in Parquet payload"))??;

    Ok(record_batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    #[test]
    fn test_load_trades() {
        // This test assumes that the `ingester` has been run and `test_data.mmap`
        // exists in the workspace root.
        let mut path = env::current_dir().unwrap();
        // The `data` crate is in `flowsurface/data`, but `test_data.mmap` is in `flowsurface/`.
        // So we need to go up one level.
        path.pop(); // From flowsurface/data to flowsurface/
        path.push("test_data.mmap");

        if !path.exists() {
            // Fallback for different test execution environments.
            // If test is run from root, current_dir is flowsurface/
            path = env::current_dir().unwrap();
            path.push("test_data.mmap");
        }
        
        println!("Attempting to load from: {}", path.display());
        assert!(path.exists(), "test_data.mmap not found. Run the `ingester` binary first.");

        let key = "trades/BTCUSDT/2025-11-20";
        let result = load_trades_from_mmap(&path, key);

        assert!(result.is_ok(), "Failed to load trades: {:?}", result.err());

        let record_batch = result.unwrap();
        assert_eq!(record_batch.num_rows(), 1000);
        assert_eq!(record_batch.num_columns(), 4);
    }
}
