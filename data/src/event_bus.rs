//! Unified event bus for data service layer.
//!
//! This module provides a centralized event system for communication between
//! data services, allowing components to publish and subscribe to events
//! without tight coupling.
//!
//! # Usage
//!
//! ```rust
//! use data::event_bus::{EventBus, DataEvent};
//!
//! // Create event bus
//! let event_bus = EventBus::new();
//!
//! // Subscribe to events
//! let mut receiver = event_bus.subscribe();
//!
//! // Publish events
//! event_bus.publish(DataEvent::DownloadStarted {
//!     symbol: "BTCUSDT".to_string(),
//!     date: "2025-11-25".to_string(),
//! });
//!
//! // Receive events
//! tokio::spawn(async move {
//!     while let Ok(event) = receiver.recv().await {
//!         println!("Received event: {:?}", event);
//!     }
//! });
//! ```

use tokio::sync::broadcast;

/// Unified event type for data service layer.
///
/// This enum represents all possible events that can occur in the data layer,
/// including download events, availability changes, and data updates.
#[derive(Debug, Clone)]
pub enum DataEvent {
    // ========== Download Events ==========
    
    /// A download task has been requested.
    DownloadRequested {
        symbol: String,
        date: String,
        data_type: DataType,
        priority: DownloadPriority,
    },
    
    /// A download has started.
    DownloadStarted {
        symbol: String,
        date: String,
        data_type: DataType,
    },
    
    /// Download progress update (0.0 to 1.0).
    DownloadProgress {
        symbol: String,
        date: String,
        data_type: DataType,
        progress: f32,
    },
    
    /// A download has completed successfully.
    DownloadCompleted {
        symbol: String,
        date: String,
        data_type: DataType,
    },
    
    /// A download has failed.
    DownloadFailed {
        symbol: String,
        date: String,
        data_type: DataType,
        error: String, // Error message (DataError doesn't implement Clone)
        retry_count: u32,
    },
    
    // ========== Availability Events ==========
    
    /// Data availability status has changed.
    AvailabilityChanged {
        symbol: String,
        date: String,
        status: DataAvailability,
    },
    
    // ========== Data Events ==========
    
    /// Data has been updated (new data available in cache).
    DataUpdated {
        symbol: String,
        date: String,
        data_type: DataType,
    },
}

/// Type of data being downloaded or updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataType {
    Tick,
    Kline { timeframe: Option<String> },
}

impl DataType {
    pub fn as_str(&self) -> &'static str {
        match self {
            DataType::Tick => "tick",
            DataType::Kline { .. } => "kline",
        }
    }
}

/// Download priority level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DownloadPriority {
    /// User-initiated request (highest priority).
    UserRequest = 0,
    /// Preload request (medium priority).
    Preload = 1,
    /// Background task (lowest priority).
    Background = 2,
}

/// Data availability status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataAvailability {
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

/// Unified event bus for data service layer.
///
/// This is a thin wrapper around `tokio::sync::broadcast::Sender` that provides
/// a convenient API for publishing and subscribing to events.
///
/// The event bus uses a broadcast channel, which means:
/// - Multiple subscribers can receive the same event
/// - Events are sent to all active subscribers
/// - If a subscriber is slow, it may miss events (backpressure)
///
/// # Example
///
/// ```rust
/// use data::event_bus::{EventBus, DataEvent, DataType, DownloadPriority};
///
/// let event_bus = EventBus::new();
///
/// // Subscribe
/// let mut receiver = event_bus.subscribe();
///
/// // Publish
/// event_bus.publish(DataEvent::DownloadRequested {
///     symbol: "BTCUSDT".to_string(),
///     date: "2025-11-25".to_string(),
///     data_type: DataType::Tick,
///     priority: DownloadPriority::UserRequest,
/// });
///
/// // Receive (in async context)
/// tokio::spawn(async move {
///     while let Ok(event) = receiver.recv().await {
///         match event {
///             DataEvent::DownloadRequested { symbol, date, .. } => {
///                 println!("Download requested: {} {}", symbol, date);
///             }
///             _ => {}
///         }
///     }
/// });
/// ```
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<DataEvent>,
}

impl EventBus {
    /// Creates a new event bus with a default channel capacity of 1000.
    pub fn new() -> Self {
        Self::with_capacity(1000)
    }
    
    /// Creates a new event bus with the specified channel capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` - The maximum number of events that can be queued.
    ///   If the channel is full, publishing will fail.
    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }
    
    /// Subscribes to events from this event bus.
    ///
    /// Returns a receiver that can be used to receive events.
    /// Multiple subscribers can be created from the same event bus.
    ///
    /// # Example
    ///
    /// ```rust
    /// let event_bus = EventBus::new();
    /// let mut receiver = event_bus.subscribe();
    ///
    /// // In async context:
    /// while let Ok(event) = receiver.recv().await {
    ///     // Handle event
    /// }
    /// ```
    pub fn subscribe(&self) -> broadcast::Receiver<DataEvent> {
        self.sender.subscribe()
    }
    
    /// Publishes an event to all subscribers.
    ///
    /// Returns the number of active subscribers that received the event.
    /// Returns an error if the channel is closed (should not happen in normal operation).
    ///
    /// # Example
    ///
    /// ```rust
    /// let event_bus = EventBus::new();
    /// event_bus.publish(DataEvent::DownloadStarted {
    ///     symbol: "BTCUSDT".to_string(),
    ///     date: "2025-11-25".to_string(),
    ///     data_type: DataType::Tick,
    /// });
    /// ```
    pub fn publish(&self, event: DataEvent) -> Result<usize, broadcast::error::SendError<DataEvent>> {
        self.sender.send(event)
    }
    
    /// Attempts to publish an event without blocking.
    ///
    /// Returns `Ok(count)` if the event was sent to `count` subscribers,
    /// or `Err(event)` if the channel is full or closed.
    ///
    /// Note: `tokio::sync::broadcast` doesn't have a non-blocking `try_send` method.
    /// This method is a placeholder for future implementation if needed.
    /// For now, use `publish` which will handle backpressure automatically.
    #[allow(unused)]
    pub fn try_publish(&self, event: DataEvent) -> Result<usize, broadcast::error::SendError<DataEvent>> {
        // Broadcast channel doesn't have try_send, so we just call send
        // In practice, broadcast channels handle backpressure by dropping slow subscribers
        self.sender.send(event)
    }
    
    /// Returns the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};
    
    #[tokio::test]
    async fn test_event_bus_basic() {
        let event_bus = EventBus::new();
        let mut receiver = event_bus.subscribe();
        
        // Publish event
        event_bus.publish(DataEvent::DownloadStarted {
            symbol: "BTCUSDT".to_string(),
            date: "2025-11-25".to_string(),
            data_type: DataType::Tick,
        }).unwrap();
        
        // Receive event
        let event = receiver.recv().await.unwrap();
        match event {
            DataEvent::DownloadStarted { symbol, date, .. } => {
                assert_eq!(symbol, "BTCUSDT");
                assert_eq!(date, "2025-11-25");
            }
            _ => panic!("Unexpected event type"),
        }
    }
    
    #[tokio::test]
    async fn test_multiple_subscribers() {
        let event_bus = EventBus::new();
        let mut receiver1 = event_bus.subscribe();
        let mut receiver2 = event_bus.subscribe();
        
        event_bus.publish(DataEvent::DownloadCompleted {
            symbol: "ETHUSDT".to_string(),
            date: "2025-11-25".to_string(),
            data_type: DataType::Tick,
        }).unwrap();
        
        // Both receivers should get the event
        let event1 = receiver1.recv().await.unwrap();
        let event2 = receiver2.recv().await.unwrap();
        
        match (event1, event2) {
            (
                DataEvent::DownloadCompleted { symbol: s1, .. },
                DataEvent::DownloadCompleted { symbol: s2, .. },
            ) => {
                assert_eq!(s1, "ETHUSDT");
                assert_eq!(s2, "ETHUSDT");
            }
            _ => panic!("Unexpected event types"),
        }
    }
    
    #[tokio::test]
    async fn test_subscriber_count() {
        let event_bus = EventBus::new();
        assert_eq!(event_bus.subscriber_count(), 0);
        
        let _receiver1 = event_bus.subscribe();
        assert_eq!(event_bus.subscriber_count(), 1);
        
        let _receiver2 = event_bus.subscribe();
        assert_eq!(event_bus.subscriber_count(), 2);
    }
}

