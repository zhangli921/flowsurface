//! Historical data download coordinator with priority queue and retry mechanism.
//!
//! This coordinator manages download tasks for historical data, implementing:
//! - Priority queue for task scheduling
//! - Retry mechanism with exponential backoff
//! - Integration with DataAvailabilityIndex
//! - Event publishing for UI updates
//!
//! The coordinator coordinates between the task queue, executor, availability index,
//! and event bus, but does not directly execute downloads.

use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::{
    data_availability_index::DataAvailabilityIndex,
    data_error::DataError,
    event_bus::{DataAvailability, DataEvent, DataType, DownloadPriority, EventBus},
    historical_download_executor::HistoricalDownloadExecutor,
};

/// Download task for historical data.
#[derive(Debug, Clone)]
pub struct DownloadTask {
    pub symbol: String,
    pub date: String,
    pub data_type: DataType,
    pub timeframe: Option<String>, // For Kline
    pub priority: DownloadPriority,
    pub retry_count: u32,
    pub created_at: Instant,
}

impl DownloadTask {
    /// Creates a new download task.
    pub fn new(
        symbol: String,
        date: String,
        data_type: DataType,
        timeframe: Option<String>,
        priority: DownloadPriority,
    ) -> Self {
        Self {
            symbol,
            date,
            data_type,
            timeframe,
            priority,
            retry_count: 0,
            created_at: Instant::now(),
        }
    }

    /// Gets a unique key for this task.
    pub fn key(&self) -> String {
        match &self.data_type {
            DataType::Tick => format!("ticks:{}_{}", self.symbol, self.date),
            DataType::Kline { timeframe } => {
                let tf = timeframe.as_deref().unwrap_or("1m");
                format!("kline:{}_{}_{}", self.symbol, self.date, tf)
            }
        }
    }
}

/// Wrapper for DownloadTask to implement priority ordering.
///
/// Higher priority (lower numeric value) and earlier creation time come first.
#[derive(Debug)]
struct PrioritizedTask {
    task: DownloadTask,
}

impl PartialEq for PrioritizedTask {
    fn eq(&self, other: &Self) -> bool {
        self.task.priority == other.task.priority
            && self.task.created_at == other.task.created_at
    }
}

impl Eq for PrioritizedTask {}

impl PartialOrd for PrioritizedTask {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrioritizedTask {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // First compare by priority (lower value = higher priority)
        match other.task.priority.cmp(&self.task.priority) {
            std::cmp::Ordering::Equal => {
                // Then by creation time (earlier = higher priority)
                self.task.created_at.cmp(&other.task.created_at)
            }
            other => other,
        }
    }
}

/// Coordinator for managing historical data downloads.
///
/// This coordinator coordinates download tasks, retry logic, availability index updates,
/// and event publishing. It delegates actual download execution to `HistoricalDownloadExecutor`.
pub struct HistoricalDownloadCoordinator {
    task_queue: Arc<Mutex<BinaryHeap<PrioritizedTask>>>,
    availability_index: Arc<DataAvailabilityIndex>,
    executor: Arc<HistoricalDownloadExecutor>,
    event_bus: Arc<EventBus>,
    max_concurrent_downloads: usize,
    current_downloads: Arc<Mutex<HashSet<String>>>, // Task keys currently being downloaded
    shutdown: Arc<tokio::sync::Notify>,
}

impl HistoricalDownloadCoordinator {
    /// Creates a new `HistoricalDownloadCoordinator`.
    ///
    /// Returns the coordinator and a handle to the download loop task.
    /// The download loop should be spawned as a background task.
    pub fn new(
        availability_index: Arc<DataAvailabilityIndex>,
        executor: Arc<HistoricalDownloadExecutor>,
        event_bus: Arc<EventBus>,
    ) -> Self {
        Self {
            task_queue: Arc::new(Mutex::new(BinaryHeap::new())),
            availability_index,
            executor,
            event_bus,
            max_concurrent_downloads: 1,
            current_downloads: Arc::new(Mutex::new(HashSet::new())),
            shutdown: Arc::new(tokio::sync::Notify::new()),
        }
    }
    
    /// Gets a reference to the executor (for accessing cache_dir).
    pub fn executor(&self) -> &Arc<HistoricalDownloadExecutor> {
        &self.executor
    }
    

    /// Sets the maximum number of concurrent downloads.
    pub fn set_max_concurrent_downloads(&mut self, max: usize) {
        self.max_concurrent_downloads = max;
    }

    /// Submits a download task to the queue.
    pub async fn submit_task(&self, task: DownloadTask) {
        let key = task.key();
        
        // Check if already downloading
        {
            let current = self.current_downloads.lock().await;
            if current.contains(&key) {
                log::debug!("Task {} already in progress, skipping", key);
                return;
            }
        }
        
        // Check availability index
        let status = self.availability_index.check_availability(&task.symbol, &task.date).await;
        if status == DataAvailability::Available {
            log::debug!("Data already available for {} {}, skipping", task.symbol, task.date);
            return;
        }
        
        log::info!("[DownloadCoordinator] Submitting download task: {} {} ({:?})", 
            task.symbol, task.date, task.data_type);
        
        // Update index to Downloading
        self.availability_index
            .update_availability(&task.symbol, &task.date, DataAvailability::Downloading)
            .await;
        
        // Publish event
        self.event_bus.publish(DataEvent::DownloadRequested {
            symbol: task.symbol.clone(),
            date: task.date.clone(),
            data_type: task.data_type.clone(),
            priority: task.priority,
        }).ok();
        
        // Add to queue
        let mut queue = self.task_queue.lock().await;
        let queue_size = queue.len();
        queue.push(PrioritizedTask { task });
        drop(queue);
        
        log::info!("[DownloadCoordinator] Task added to queue. Queue size: {}", queue_size + 1);
        
        // Notify download loop
        self.shutdown.notify_one();
    }

    /// Runs the download loop (should be spawned as a background task).
    pub async fn run_download_loop(&self) {
        log::info!("[DownloadCoordinator] Download loop started");
        loop {
            // Wait for tasks or shutdown signal
            tokio::select! {
                _ = self.shutdown.notified() => {
                    // Check for tasks
                }
                _ = sleep(Duration::from_millis(100)) => {
                    // Periodic check
                }
            }
            
            // Check if we can start a new download
            let (can_start, current_count, queue_size) = {
                let current = self.current_downloads.lock().await;
                let queue = self.task_queue.lock().await;
                let can = current.len() < self.max_concurrent_downloads;
                (can, current.len(), queue.len())
            };
            
            if !can_start {
                if queue_size > 0 {
                    log::debug!("[DownloadCoordinator] Waiting for slot. Current: {}/{}, Queue: {}", 
                        current_count, self.max_concurrent_downloads, queue_size);
                }
                continue;
            }
            
            // Get next task from queue
            let task = {
                let mut queue = self.task_queue.lock().await;
                queue.pop().map(|pt| pt.task)
            };
            
            if let Some(task) = task {
                let key = task.key();
                log::info!("[DownloadCoordinator] Starting download: {} {} ({:?})", 
                    task.symbol, task.date, task.data_type);
                
                // Mark as downloading
                {
                    let mut current = self.current_downloads.lock().await;
                    current.insert(key.clone());
                }
                
                // Spawn download task
                let service = self.clone_for_download();
                let task_key = key.clone();
                let task_symbol = task.symbol.clone();
                let task_date = task.date.clone();
                tokio::spawn(async move {
                    log::info!("[DownloadCoordinator] Executing download: {} {}", task_symbol, task_date);
                    let result = service.download_with_retry(task.clone()).await;
                    
                    // Remove from current downloads
                    {
                        let mut current = service.current_downloads.lock().await;
                        current.remove(&task_key);
                    }
                    
                    // Handle result
                    match result {
                        Ok(_) => {
                            log::info!("[DownloadCoordinator] Download completed: {} {}", task.symbol, task.date);
                            // Update index to Available
                            service.availability_index
                                .update_availability(&task.symbol, &task.date, DataAvailability::Available)
                                .await;
                            
                            // Publish event
                            service.event_bus.publish(DataEvent::DownloadCompleted {
                                symbol: task.symbol.clone(),
                                date: task.date.clone(),
                                data_type: task.data_type.clone(),
                            }).ok();
                        }
                        Err(e) => {
                            // Handle error (retry logic is in download_with_retry)
                            log::warn!("[DownloadCoordinator] Download failed for {} {}: {}", task.symbol, task.date, e);
                        }
                    }
                });
            }
        }
    }

    /// Creates a clone of self for use in async tasks.
    fn clone_for_download(&self) -> DownloadCoordinatorClone {
        DownloadCoordinatorClone {
            availability_index: self.availability_index.clone(),
            executor: self.executor.clone(),
            event_bus: self.event_bus.clone(),
            current_downloads: self.current_downloads.clone(),
        }
    }
}

/// Clone-friendly wrapper for download coordinator.
struct DownloadCoordinatorClone {
    availability_index: Arc<DataAvailabilityIndex>,
    executor: Arc<HistoricalDownloadExecutor>,
    event_bus: Arc<EventBus>,
    current_downloads: Arc<Mutex<HashSet<String>>>,
}

impl DownloadCoordinatorClone {
    async fn download_with_retry(&self, mut task: DownloadTask) -> Result<(), DataError> {
        const MAX_RETRIES: u32 = 3;
        
        // Publish download started event
        self.event_bus.publish(DataEvent::DownloadStarted {
            symbol: task.symbol.clone(),
            date: task.date.clone(),
            data_type: task.data_type.clone(),
        }).ok();
        
        loop {
            let result = match &task.data_type {
                DataType::Tick => {
                    self.executor.download_and_cache_ticks(&task.symbol, &task.date).await
                        .map(|_| ())
                }
                DataType::Kline { timeframe } => {
                    let tf = timeframe.as_deref().unwrap_or("1m");
                    self.executor.download_and_cache_kline(&task.symbol, &task.date, tf).await
                        .map(|_| ())
                }
            };
            
            match result {
                Ok(_) => {
                    return Ok(());
                }
                Err(e) => {
                    task.retry_count += 1;
                    
                    if task.retry_count > MAX_RETRIES {
                        self.availability_index
                            .update_availability(&task.symbol, &task.date, DataAvailability::Unavailable)
                            .await;
                        
                        self.event_bus.publish(DataEvent::DownloadFailed {
                            symbol: task.symbol.clone(),
                            date: task.date.clone(),
                            data_type: task.data_type.clone(),
                            error: e.to_string(),
                            retry_count: task.retry_count,
                        }).ok();
                        
                        return Err(e);
                    }
                    
                    let delay = self.calculate_retry_delay(&e, task.retry_count);
                    
                    self.event_bus.publish(DataEvent::DownloadFailed {
                        symbol: task.symbol.clone(),
                        date: task.date.clone(),
                        data_type: task.data_type.clone(),
                        error: format!("Retrying: {}", e),
                        retry_count: task.retry_count,
                    }).ok();
                    
                    sleep(delay).await;
                }
            }
        }
    }

    fn calculate_retry_delay(&self, error: &DataError, attempt: u32) -> Duration {
        let error_str = error.to_string().to_lowercase();
        
        if error_str.contains("404") || error_str.contains("not found") {
            return Duration::from_secs(3600 * (1 << (attempt.saturating_sub(1))));
        }
        
        if error_str.contains("403") || error_str.contains("forbidden") {
            return Duration::from_secs(1);
        }
        
        Duration::from_secs(1 << (attempt.saturating_sub(1)))
    }
}

