//! In-memory cache for real-time K-line data.
//!
//! This module provides a shared cache for K-line data that can be accessed
//! by both RealtimeIngesterService (for writing) and RealtimeDataService (for reading).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use crate::kline::KLine;

/// Cache key: (symbol, timeframe)
type CacheKey = (String, String);

/// Cache entry: maps open_time_us to KLine
type CacheEntry = BTreeMap<u64, KLine>;

/// Shared in-memory cache for real-time K-line data.
///
/// This cache stores K-line data organized by symbol and timeframe.
/// It is thread-safe and can be shared between multiple services.
#[derive(Clone)]
pub struct KlineCache {
    inner: Arc<Mutex<BTreeMap<CacheKey, CacheEntry>>>,
}

impl KlineCache {
    /// Creates a new empty K-line cache.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Inserts or updates a K-line in the cache.
    pub fn insert(&self, symbol: &str, timeframe: &str, kline: KLine) {
        let key = (symbol.to_string(), timeframe.to_string());
        let mut cache = self.inner.lock().unwrap();
        let entry = cache.entry(key).or_insert_with(BTreeMap::new);
        entry.insert(kline.open_time_us, kline);
    }

    /// Inserts multiple K-lines into the cache.
    pub fn insert_many(&self, symbol: &str, timeframe: &str, klines: &[KLine]) {
        let key = (symbol.to_string(), timeframe.to_string());
        let mut cache = self.inner.lock().unwrap();
        let entry = cache.entry(key).or_insert_with(BTreeMap::new);
        for kline in klines {
            entry.insert(kline.open_time_us, kline.clone());
        }
    }

    /// Retrieves K-lines within the specified time range.
    ///
    /// Returns a vector of K-lines sorted by open_time_us.
    pub fn get_range(&self, symbol: &str, timeframe: &str, start_us: u64, end_us: u64) -> Vec<KLine> {
        let key = (symbol.to_string(), timeframe.to_string());
        let cache = self.inner.lock().unwrap();
        
        if let Some(entry) = cache.get(&key) {
            entry
                .range(start_us..end_us)
                .map(|(_, kline)| kline.clone())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Clears all K-lines for a specific symbol and timeframe.
    pub fn clear(&self, symbol: &str, timeframe: &str) {
        let key = (symbol.to_string(), timeframe.to_string());
        let mut cache = self.inner.lock().unwrap();
        cache.remove(&key);
    }

    /// Clears all cached data.
    pub fn clear_all(&self) {
        let mut cache = self.inner.lock().unwrap();
        cache.clear();
    }

    /// Returns the number of cached K-lines for a specific symbol and timeframe.
    pub fn len(&self, symbol: &str, timeframe: &str) -> usize {
        let key = (symbol.to_string(), timeframe.to_string());
        let cache = self.inner.lock().unwrap();
        cache.get(&key).map(|e| e.len()).unwrap_or(0)
    }

    /// Returns the time range of cached K-lines for a specific symbol and timeframe.
    ///
    /// Returns (min_time, max_time) if data exists, None otherwise.
    pub fn time_range(&self, symbol: &str, timeframe: &str) -> Option<(u64, u64)> {
        let key = (symbol.to_string(), timeframe.to_string());
        let cache = self.inner.lock().unwrap();
        
        if let Some(entry) = cache.get(&key) {
            if let (Some(first), Some(last)) = (entry.first_key_value(), entry.last_key_value()) {
                Some((*first.0, *last.0))
            } else {
                None
            }
        } else {
            None
        }
    }
}

impl Default for KlineCache {
    fn default() -> Self {
        Self::new()
    }
}

