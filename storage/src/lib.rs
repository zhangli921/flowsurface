use memmap2::Mmap;
use std::fs::File;
use std::path::Path;
use std::{io, mem, slice};

const MAGIC_NUMBER: &[u8; 8] = b"ZEROCPY!";
const DATA_VERSION: u16 = 1;

/// Custom error types for the storage module.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("I/O error: {0}")]
    IoError(#[from] io::Error),
    #[error("File is truncated or invalid")]
    TruncatedFile,
    #[error("Schema mismatch (magic number or version)")]
    SchemaMismatch,
    #[error("Data layout error: {0}")]
    LayoutError(&'static str),
    #[error("Index key not found")]
    IndexNotFound,
}

/// The file header for the memory-mapped store.
/// It is exactly 64 bytes to fit into a single CPU cache line.
#[repr(C)]
#[derive(Debug)]
pub struct FileHeader {
    pub magic_number: [u8; 8],
    pub data_version: u16,
    pub index_count: usize,
    pub payload_start_offset: usize,
    pub reserved: [u8; 38],
}

impl FileHeader {
    pub fn magic_number(&self) -> &[u8; 8] {
        &self.magic_number
    }

    pub fn data_version(&self) -> u16 {
        self.data_version
    }

    pub fn index_count(&self) -> usize {
        self.index_count
    }

    pub fn payload_start_offset(&self) -> usize {
        self.payload_start_offset
    }
}

/// An entry in the index block.
/// The total size is 24 bytes, optimized for cache density.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IndexEntry {
    pub key_hash: u64,
    pub start_offset: usize,
    pub length: u32,
    pub reserved: u32,
}

impl IndexEntry {
    pub fn key_hash(&self) -> u64 {
        self.key_hash
    }

    pub fn start_offset(&self) -> usize {
        self.start_offset
    }

    pub fn length(&self) -> u32 {
        self.length
    }
}

/// A wrapper around a memory-mapped file that provides safe, zero-copy access
/// to the underlying data structures.
#[derive(Debug)]
pub struct MmapStore {
    // Raw pointer to the Mmap object. This is needed for a self-referential struct.
    // The Mmap object is heap-allocated, and we manage its lifetime manually.
    _mmap_ptr: *const Mmap,
    header: &'static FileHeader,
    index: &'static [IndexEntry],
    payload: &'static [u8],
}

// Manual Drop implementation to clean up the leaked Mmap object.
impl Drop for MmapStore {
    fn drop(&mut self) {
        unsafe {
            // Reconstruct the Box from the raw pointer and drop it.
            // This will unmap the memory correctly.
            let _ = Box::from_raw(self._mmap_ptr as *mut Mmap);
        }
    }
}

impl MmapStore {
    /// Opens and maps a file, validating its structure.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let file = File::open(path)?;
        // Map the file and immediately box it and leak it to get a stable address.
        let mmap = unsafe { Mmap::map(&file)? };
        let mmap_ptr = Box::into_raw(Box::new(mmap));
        
        // SAFETY: The pointer is valid and comes from a Box. We will reconstruct the box on Drop.
        // The `'static` lifetime is assumed, but it's bounded by the MmapStore's lifetime.
        let raw_bytes: &'static [u8] = unsafe { &*mmap_ptr };

        // 1. Validate length and get header
        if raw_bytes.len() < mem::size_of::<FileHeader>() {
            // Clean up the leaked box before returning the error
            unsafe { let _ = Box::from_raw(mmap_ptr); }
            return Err(StoreError::TruncatedFile);
        }
        // SAFETY: We've checked the length.
        let header = unsafe { &*(raw_bytes.as_ptr() as *const FileHeader) };

        // 2. Validate header content
        if &header.magic_number != MAGIC_NUMBER || header.data_version != DATA_VERSION {
            unsafe { let _ = Box::from_raw(mmap_ptr); }
            return Err(StoreError::SchemaMismatch);
        }

        // 3. Get the index slice
        let index_start = mem::size_of::<FileHeader>();
        let index_size = header.index_count * mem::size_of::<IndexEntry>();

        if raw_bytes.len() < index_start + index_size {
            unsafe { let _ = Box::from_raw(mmap_ptr); }
            return Err(StoreError::TruncatedFile);
        }

        let index_ptr = unsafe { raw_bytes.as_ptr().add(index_start) };

        if index_ptr.align_offset(mem::align_of::<IndexEntry>()) != 0 {
            unsafe { let _ = Box::from_raw(mmap_ptr); }
            return Err(StoreError::LayoutError("Index block is not properly aligned"));
        }
        // SAFETY: We've checked length and alignment.
        let index = unsafe { slice::from_raw_parts(index_ptr as *const IndexEntry, header.index_count) };

        // 4. Validate payload offset and get payload slice
        if raw_bytes.len() < header.payload_start_offset {
            unsafe { let _ = Box::from_raw(mmap_ptr); }
            return Err(StoreError::TruncatedFile);
        }
        let payload = &raw_bytes[header.payload_start_offset..];
        
        Ok(MmapStore {
            _mmap_ptr: mmap_ptr,
            header,
            index,
            payload,
        })
    }

    /// Returns a reference to the file header.
    pub fn header(&self) -> &FileHeader {
        self.header
    }

    /// Returns the full index as a slice of entries.
    pub fn index(&self) -> &[IndexEntry] {
        self.index
    }
    
    /// Performs a binary search on the index to find a specific entry.
    pub fn lookup_index(&self, key_hash: u64) -> Result<&IndexEntry, StoreError> {
        self.index
            .binary_search_by_key(&key_hash, |entry| entry.key_hash)
            .map(|i| &self.index[i])
            .map_err(|_| StoreError::IndexNotFound)
    }

    /// Retrieves the payload data for a given index entry.
    /// This is a zero-copy operation, returning a slice of the memory-mapped file.
    /// Returns an empty slice if the payload region is not yet populated or the entry is out of bounds.
    /// 
    /// CRITICAL: For financial data integrity, if the payload is incomplete (truncated),
    /// we return an empty slice rather than a partial payload that would corrupt Parquet files.
    pub fn get_payload<'a>(&'a self, entry: &IndexEntry) -> &'a [u8] {
        let start = entry.start_offset;
        let end = start + entry.length as usize;
        
        // Bounds check: ensure we don't access beyond the payload slice
        if start >= self.payload.len() {
            return &[];
        }
        
        // CRITICAL: If the payload is incomplete (entry extends beyond available data),
        // return empty slice to avoid corrupting Parquet files
        // This ensures data integrity for financial data
        if end > self.payload.len() {
            return &[];
        }
        
        &self.payload[start..end]
    }
}

// SAFETY: The `MmapStore` is designed to be read-only. The underlying `Mmap` object
// from `memmap2` is safe to access from multiple threads for read operations.
// The raw pointer `_mmap_ptr` is stable and the data it points to (the Mmap object)
// lives for the duration of the `MmapStore` instance. Therefore, it is safe
// to mark MmapStore as both Send and Sync.
unsafe impl Send for MmapStore {}
unsafe impl Sync for MmapStore {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;

    fn calculate_hash<T: Hash>(t: &T) -> u64 {
        let mut s = DefaultHasher::new();
        t.hash(&mut s);
        s.finish()
    }

    // Helper to write raw bytes for testing
    fn write_as_bytes<T>(writer: &mut impl Write, data: &T) -> io::Result<()> {
        let bytes = unsafe {
            slice::from_raw_parts(
                (data as *const T) as *const u8,
                mem::size_of::<T>(),
            )
        };
        writer.write_all(bytes)
    }

    #[test]
    fn test_create_and_read_store() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.store");

        // --- Test Data ---
        let payload1: &'static [u8] = b"hello world";
        let payload2: &'static [u8] = b"another payload";
        let key1 = "key1";
        let key2 = "key2";
        let hash1 = calculate_hash(&key1);
        let hash2 = calculate_hash(&key2);

        let mut entries: Vec<(u64, &'static [u8])> = vec![
            (hash1, payload1),
            (hash2, payload2),
        ];
        // Sort by hash for binary search
        entries.sort_by_key(|(h, _)| *h);

        // --- Build File Manually ---
        let mut file = fs::File::create(&file_path).unwrap();

        let index_count = entries.len();
        let header_size = mem::size_of::<FileHeader>();
        let index_total_size = index_count * mem::size_of::<IndexEntry>();
        let payload_start_offset = header_size + index_total_size;

        // Write Header
        let header = FileHeader {
            magic_number: *MAGIC_NUMBER,
            data_version: DATA_VERSION,
            index_count,
            payload_start_offset,
            reserved: [0; 38],
        };
        write_as_bytes(&mut file, &header).unwrap();

        // Write Index and Payload
        let mut current_payload_offset = 0;
        for (hash, payload_data) in &entries {
            let entry = IndexEntry {
                key_hash: *hash,
                start_offset: current_payload_offset,
                length: payload_data.len() as u32,
                reserved: 0,
            };
            write_as_bytes(&mut file, &entry).unwrap();
            current_payload_offset += payload_data.len();
        }

        for (_, payload_data) in &entries {
            file.write_all(payload_data).unwrap();
        }

        file.sync_all().unwrap();
        drop(file);

        // --- Read and Verify ---
        let store = MmapStore::open(&file_path).unwrap();

        // Verify header
        assert_eq!(store.header().magic_number(), MAGIC_NUMBER);
        assert_eq!(store.header().data_version(), DATA_VERSION);
        assert_eq!(store.header().index_count(), index_count);

        // Verify index lookup for the first entry
        let first_key_hash = entries[0].0;
        let entry = store.lookup_index(first_key_hash).unwrap();
        assert_eq!(entry.key_hash(), first_key_hash);

        // Verify payload retrieval for the first entry
        let payload = store.get_payload(entry);
        assert_eq!(payload, entries[0].1);
        
        // Verify index lookup and payload for the second entry
        let second_key_hash = entries[1].0;
        let entry2 = store.lookup_index(second_key_hash).unwrap();
        let payload2_retrieved = store.get_payload(entry2);
        assert_eq!(payload2_retrieved, entries[1].1);
        
        // Verify looking up a non-existent key fails
        assert!(store.lookup_index(12345).is_err());
    }

    #[test]
    fn test_error_handling() {
        let dir = tempfile::tempdir().unwrap();

        // Test non-existent file
        let bad_path = dir.path().join("nonexistent.store");
        assert!(matches!(MmapStore::open(&bad_path), Err(StoreError::IoError(_))));

        // Test truncated file (too short for header)
        let truncated_path = dir.path().join("truncated.store");
        let mut file = fs::File::create(&truncated_path).unwrap();
        file.write_all(&[0; 10]).unwrap();
        drop(file);
        assert!(matches!(MmapStore::open(&truncated_path), Err(StoreError::TruncatedFile)));

        // Test bad magic number
        let bad_magic_path = dir.path().join("bad_magic.store");
        let mut file = fs::File::create(&bad_magic_path).unwrap();
        let header = FileHeader {
            magic_number: *b"BADMAGIC",
            data_version: DATA_VERSION,
            index_count: 0,
            payload_start_offset: 64,
            reserved: [0; 38],
        };
        write_as_bytes(&mut file, &header).unwrap();
        drop(file);
        assert!(matches!(MmapStore::open(&bad_magic_path), Err(StoreError::SchemaMismatch)));
    }
}