//! Defines the canonical K-line data structure for the application.

use exchange::Kline as ExchangeKline;

/// Represents a single OHLCV (Open, High, Low, Close, Volume) data point,
/// commonly known as a "candlestick".
///
/// This struct uses primitive types and is designed to be easily clonable
/// and copyable, making it suitable for use in `iced` messages and for
/// high-performance aggregation tasks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KLine {
    /// The start time of the K-line period, as a Unix timestamp in nanoseconds.
    /// This serves as the primary key.
    pub open_time_ns: u64,
    /// The opening price for the period.
    pub open: f64,
    /// The highest price reached during the period.
    pub high: f64,
    /// The lowest price reached during the period.
    pub low: f64,
    /// The closing price for the period.
    pub close: f64,
    /// The total volume traded during the period.
    pub volume: f64,
    /// The total number of trades that occurred during the period.
    pub num_trades: u32,
}

impl From<ExchangeKline> for KLine {
    fn from(kline: ExchangeKline) -> Self {
        Self {
            // Convert ms to ns
            open_time_ns: kline.time * 1_000_000,
            open: kline.open.to_f32() as f64,
            high: kline.high.to_f32() as f64,
            low: kline.low.to_f32() as f64,
            close: kline.close.to_f32() as f64,
            volume: (kline.volume.0 + kline.volume.1) as f64,
            // exchange::Kline doesn't have num_trades, so default to 0
            num_trades: 0,
        }
    }
}