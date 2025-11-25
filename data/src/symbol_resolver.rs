//! Unified symbol resolver.
//!
//! This resolver provides:
//! - Symbol normalization (converting various formats to exchange-specific format)
//! - Exchange type inference from symbols
//! - Cache key generation
//! - Symbol validation

use exchange::Exchange;

/// Unified symbol resolver.
///
/// This resolver centralizes all symbol-related operations to ensure consistency
/// across the application. It resolves symbols to their normalized form and
/// provides utilities for cache key generation and exchange inference.
#[derive(Clone, Default)]
pub struct SymbolResolver;

impl SymbolResolver {
    /// Creates a new `SymbolResolver`.
    pub fn new() -> Self {
        Self
    }

    /// Normalizes a symbol to the standard format for a given exchange.
    ///
    /// Handles various input formats:
    /// - "BTC" → "BTCUSDT" (for Binance Spot)
    /// - "SOL" → "SOLUSDT"
    /// - "BTCUSDT" → "BTCUSDT" (unchanged)
    /// - "ETH-USDT-SWAP" → "ETHUSDT"
    ///
    /// # Arguments
    ///
    /// * `symbol` - The symbol to normalize (e.g., "BTC", "BTCUSDT", "ETH-USDT-SWAP")
    /// * `exchange` - The target exchange (defaults to Binance Spot if None)
    ///
    /// # Returns
    ///
    /// The normalized symbol string.
    pub fn normalize(&self, symbol: &str, exchange: Option<Exchange>) -> String {
        let exchange = exchange.unwrap_or(Exchange::BinanceSpot);
        
        match exchange {
            Exchange::BinanceSpot | Exchange::BinanceLinear | Exchange::BinanceInverse => {
                self.normalize_binance_symbol(symbol)
            }
            Exchange::BybitSpot | Exchange::BybitLinear | Exchange::BybitInverse => {
                // Bybit uses similar format, but may need different handling
                self.normalize_binance_symbol(symbol) // For now, reuse Binance logic
            }
            Exchange::HyperliquidLinear | Exchange::HyperliquidSpot => {
                // Hyperliquid may use different format
                symbol.to_uppercase().replace("-", "").replace("_", "")
            }
            Exchange::OkexSpot | Exchange::OkexLinear | Exchange::OkexInverse => {
                // OKX uses different format
                symbol.to_uppercase().replace("-", "").replace("_", "")
            }
        }
    }

    /// Infers the Exchange type from a symbol.
    ///
    /// This is a best-effort inference based on symbol format and context.
    /// For ambiguous cases, defaults to Binance Spot.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The symbol to analyze
    ///
    /// # Returns
    ///
    /// The inferred Exchange type.
    pub fn infer_exchange(&self, symbol: &str) -> Exchange {
        // For now, default to Binance Spot
        // This can be enhanced with symbol registry or configuration
        // TODO: Implement proper inference based on symbol registry or user configuration
        Exchange::BinanceSpot
    }

    /// Generates a cache key for tick data.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The normalized symbol
    /// * `date` - The date in "YYYY-MM-DD" format
    ///
    /// # Returns
    ///
    /// The cache key string (e.g., "BTCUSDT_2025-11-24_ticks.parquet")
    pub fn tick_cache_key(&self, symbol: &str, date: &str) -> String {
        format!("{}_{}_ticks.parquet", symbol, date)
    }

    /// Generates a cache key for K-line data.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The normalized symbol
    /// * `date` - The date in "YYYY-MM-DD" format
    /// * `timeframe` - The timeframe (e.g., "1m", "5m", "1h")
    ///
    /// # Returns
    ///
    /// The cache key string (e.g., "BTCUSDT_2025-11-24_1m.parquet")
    pub fn kline_cache_key(&self, symbol: &str, date: &str, timeframe: &str) -> String {
        format!("{}_{}_{}.parquet", symbol, date, timeframe)
    }

    /// Generates a download key for tracking concurrent downloads.
    ///
    /// # Arguments
    ///
    /// * `data_type` - The type of data ("kline" or "ticks")
    /// * `symbol` - The normalized symbol
    /// * `date` - The date in "YYYY-MM-DD" format
    /// * `timeframe` - Optional timeframe (for K-line data)
    ///
    /// # Returns
    ///
    /// The download key string (e.g., "kline:BTCUSDT_2025-11-24_1m")
    pub fn download_key(&self, data_type: &str, symbol: &str, date: &str, timeframe: Option<&str>) -> String {
        if let Some(tf) = timeframe {
            format!("{}:{}_{}_{}", data_type, symbol, date, tf)
        } else {
            format!("{}:{}_{}", data_type, symbol, date)
        }
    }

    /// Normalizes a symbol for Binance exchange.
    ///
    /// This is the core normalization logic for Binance symbols.
    fn normalize_binance_symbol(&self, ticker: &str) -> String {
        let ticker_upper = ticker.to_uppercase();
        
        // If already in Binance format (e.g., "BTCUSDT"), return as-is
        if ticker_upper.ends_with("USDT") || ticker_upper.ends_with("USD") || ticker_upper.ends_with("BUSD") {
            return ticker_upper.replace("-", "").replace("_", "");
        }
        
        // Remove common suffixes and separators
        let normalized = ticker_upper
            .replace("-USDT-SWAP", "")
            .replace("-USDT", "")
            .replace("-USD", "")
            .replace("_PERP", "")
            .replace("-", "")
            .replace("_", "");
        
        // If it still contains separators, try to extract base and quote
        if normalized.contains("-") {
            let parts: Vec<&str> = normalized.split('-').collect();
            if parts.len() >= 2 {
                return format!("{}{}", parts[0], parts[1]);
            }
        }
        
        // If it's a pure coin name (e.g., "BTC", "ETH"), add "USDT" suffix
        // This is the default quote currency for Binance spot trading
        if normalized.len() <= 10 && normalized.chars().all(|c| c.is_alphabetic()) {
            return format!("{}USDT", normalized);
        }
        
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_binance_symbol() {
        let resolver = SymbolResolver::new();
        
        // Test various input formats
        assert_eq!(resolver.normalize("BTC", None), "BTCUSDT");
        assert_eq!(resolver.normalize("SOL", None), "SOLUSDT");
        assert_eq!(resolver.normalize("BTCUSDT", None), "BTCUSDT");
        assert_eq!(resolver.normalize("ETH-USDT-SWAP", None), "ETHUSDT");
        assert_eq!(resolver.normalize("ETH_USDT", None), "ETHUSDT");
    }

    #[test]
    fn test_cache_keys() {
        let resolver = SymbolResolver::new();
        
        assert_eq!(
            resolver.tick_cache_key("BTCUSDT", "2025-11-24"),
            "BTCUSDT_2025-11-24_ticks.parquet"
        );
        
        assert_eq!(
            resolver.kline_cache_key("BTCUSDT", "2025-11-24", "1m"),
            "BTCUSDT_2025-11-24_1m.parquet"
        );
    }

    #[test]
    fn test_download_keys() {
        let resolver = SymbolResolver::new();
        
        assert_eq!(
            resolver.download_key("ticks", "BTCUSDT", "2025-11-24", None),
            "ticks:BTCUSDT_2025-11-24"
        );
        
        assert_eq!(
            resolver.download_key("kline", "BTCUSDT", "2025-11-24", Some("1m")),
            "kline:BTCUSDT_2025-11-24_1m"
        );
    }
}

