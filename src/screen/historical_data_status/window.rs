//! Historical data status window for viewing and managing data availability.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use crate::window;
use data::{
    DataAvailability, DataEvent, DataType, DownloadPriority,
    DataAvailabilityIndex, HistoricalDownloadCoordinator, DownloadTask, EventBus,
};

use iced::{
    Alignment, Element, Length, Subscription, Task,
    widget::{button, column, container, row, scrollable, space, text},
};

/// Message type for the historical data status window.
#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    RefreshComplete(Vec<DataStatusRow>),
    DownloadSelected,
    TriggerDownload { symbol: String, date: String },
    DataEvent(DataEvent),
    Tick(Instant),
}

/// Historical data status window state.
pub struct HistoricalDataStatusWindow {
    availability_index: Arc<DataAvailabilityIndex>,
    download_coordinator: Arc<HistoricalDownloadCoordinator>,
    event_bus: Arc<EventBus>,
    window_id: window::Id,
    
    // Current display data
    data_rows: Vec<DataStatusRow>,
    selected_rows: HashSet<(String, String)>, // (symbol, date)
    
    // Refresh state
    last_refresh: Instant,
    refresh_interval: std::time::Duration,
}

/// A row in the data status table.
#[derive(Debug, Clone)]
pub struct DataStatusRow {
    pub symbol: String,
    pub date: String,
    pub status: DataAvailability,
    pub file_size: Option<u64>, // bytes
    pub last_checked: Option<Instant>,
    pub download_progress: Option<f32>, // 0.0 - 1.0
}

impl HistoricalDataStatusWindow {
    /// Creates a new historical data status window.
    pub fn new(
        availability_index: Arc<DataAvailabilityIndex>,
        download_coordinator: Arc<HistoricalDownloadCoordinator>,
        event_bus: Arc<EventBus>,
    ) -> (Self, window::Id) {
        let window_id = window::Id::unique();
        
        let window = Self {
            availability_index,
            download_coordinator,
            event_bus,
            window_id,
            data_rows: Vec::new(),
            selected_rows: HashSet::new(),
            last_refresh: Instant::now(),
            refresh_interval: std::time::Duration::from_secs(5),
        };
        
        (window, window_id)
    }

    /// Updates the window state.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => {
                return self.refresh_data();
            }
            Message::RefreshComplete(rows) => {
                self.data_rows = rows;
                self.last_refresh = Instant::now();
            }
            Message::DownloadSelected => {
                // Download all selected rows
                for (symbol, date) in &self.selected_rows {
                    let download_coordinator = self.download_coordinator.clone();
                    let symbol = symbol.clone();
                    let date = date.clone();
                    tokio::spawn(async move {
                        download_coordinator.submit_task(DownloadTask::new(
                            symbol,
                            date,
                            DataType::Tick,
                            None,
                            DownloadPriority::UserRequest,
                        )).await;
                    });
                }
            }
            Message::TriggerDownload { symbol, date } => {
                let download_coordinator = self.download_coordinator.clone();
                tokio::spawn(async move {
                    download_coordinator.submit_task(DownloadTask::new(
                        symbol,
                        date,
                        DataType::Tick,
                        None,
                        DownloadPriority::UserRequest,
                    )).await;
                });
            }
            Message::DataEvent(event) => {
                // Check if we should refresh after handling this event
                let should_refresh = matches!(event, 
                    DataEvent::DownloadCompleted { .. } | 
                    DataEvent::DownloadFailed { .. } | 
                    DataEvent::AvailabilityChanged { .. }
                );
                
                self.handle_data_event(event);
                
                // Trigger refresh after handling event to update display
                if should_refresh {
                    return self.refresh_data();
                }
            }
            Message::Tick(_) => {
                // Auto-refresh if enough time has passed
                if self.last_refresh.elapsed() >= self.refresh_interval {
                    return self.refresh_data();
                }
            }
        }
        
        Task::none()
    }

    /// Renders the window view.
    pub fn view(&self) -> Element<'_, Message> {
        let header = row![
            text("历史数据状态").size(20),
            space(),
            button("刷新").on_press(Message::Refresh),
            button("下载选中").on_press(Message::DownloadSelected),
        ]
        .spacing(10)
        .padding(10);

        let table_header = row![
            text("交易对").width(Length::Fixed(100.0)),
            text("日期").width(Length::Fixed(120.0)),
            text("状态").width(Length::Fixed(100.0)),
            text("大小").width(Length::Fixed(100.0)),
            text("操作").width(Length::Fixed(100.0)),
        ]
        .spacing(10)
        .padding(5);

        let table_rows: Vec<Element<Message>> = self.data_rows
            .iter()
            .map(|row| self.view_table_row(row))
            .collect();

        let table = column![
            table_header,
            scrollable(column(table_rows).spacing(5))
                .height(Length::Fill)
        ]
        .spacing(5);

        container(column![
            header,
            table,
        ])
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(10)
        .into()
    }

    /// Creates a subscription for events.
    pub fn subscription(&self) -> Subscription<Message> {
        // Subscribe to tick events for auto-refresh
        iced::time::every(std::time::Duration::from_secs(5))
            .map(|_| Message::Tick(Instant::now()))
    }

    /// Refreshes data from the availability index.
    fn refresh_data(&mut self) -> Task<Message> {
        log::info!("[HistoricalDataStatusWindow] Refreshing data...");
        let availability_index = self.availability_index.clone();
        let download_coordinator = self.download_coordinator.clone();
        
        Task::perform(
            async move {
                // First, scan cache directory to discover existing files
                // This ensures we show data even if index is empty
                let executor = download_coordinator.executor();
                let cache_dir = executor.cache_dir().clone();
                log::info!("[HistoricalDataStatusWindow] Scanning cache directory: {}", cache_dir.display());
                let mut discovered_symbols = std::collections::HashSet::new();
                let mut discovered_count = 0;
                
                // Scan for tick data files
                // Tick files are stored as: cache/{symbol}_{date}_ticks.parquet (flat structure)
                // Kline files are stored as: cache/{symbol}_{date}_{timeframe}.parquet (flat structure)
                if let Ok(entries) = std::fs::read_dir(&cache_dir) {
                    for entry in entries.flatten() {
                        let file_name = entry.file_name();
                        let file_name_str = file_name.to_string_lossy();
                        
                        // Check if it's a tick file: {symbol}_{date}_ticks.parquet
                        if file_name_str.ends_with("_ticks.parquet") {
                            // Extract symbol and date from filename
                            // Format: {symbol}_{date}_ticks.parquet
                            if let Some(base) = file_name_str.strip_suffix("_ticks.parquet") {
                                if let Some((symbol, date)) = base.rsplit_once('_') {
                                    let symbol = symbol.to_string();
                                    let date = date.to_string();
                                    
                                    log::debug!("[HistoricalDataStatusWindow] Found tick file: {} {}", symbol, date);
                                    discovered_symbols.insert(symbol.clone());
                                    discovered_count += 1;
                                    
                                    // Update index if not already set
                                    let current_status = availability_index
                                        .check_availability(&symbol, &date).await;
                                    if current_status == DataAvailability::Unknown {
                                        availability_index
                                            .update_availability(&symbol, &date, DataAvailability::Available)
                                            .await;
                                    }
                                } else {
                                    log::warn!("[HistoricalDataStatusWindow] Failed to parse tick filename: {}", file_name_str);
                                }
                            }
                        }
                        // Check if it's a kline file: {symbol}_{date}_{timeframe}.parquet
                        else if file_name_str.ends_with(".parquet") && !file_name_str.contains("_ticks") {
                            // Extract symbol and date from filename
                            // Format: {symbol}_{date}_{timeframe}.parquet
                            // We need to find the second-to-last underscore to separate date and timeframe
                            let parts: Vec<&str> = file_name_str.strip_suffix(".parquet")
                                .unwrap_or(&file_name_str)
                                .split('_')
                                .collect();
                            
                            if parts.len() >= 3 {
                                // Last part is timeframe, second-to-last is date, rest is symbol
                                let timeframe = parts[parts.len() - 1];
                                let date = parts[parts.len() - 2];
                                let symbol = parts[..parts.len() - 2].join("_");
                                
                                log::debug!("[HistoricalDataStatusWindow] Found kline file: {} {} {}", symbol, date, timeframe);
                                discovered_symbols.insert(symbol.clone());
                                discovered_count += 1;
                                
                                // Update index if not already set
                                let current_status = availability_index
                                    .check_availability(&symbol, date).await;
                                if current_status == DataAvailability::Unknown {
                                    availability_index
                                        .update_availability(&symbol, date, DataAvailability::Available)
                                        .await;
                                }
                            } else {
                                log::warn!("[HistoricalDataStatusWindow] Failed to parse kline filename (parts < 3): {}", file_name_str);
                            }
                        }
                        
                        // Also check for nested directory structure (legacy or alternative format)
                        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            let symbol = file_name_str.to_string();
                            let symbol_dir = entry.path();
                            
                            // Check for ticks subdirectory
                            let ticks_dir = symbol_dir.join("ticks");
                            if ticks_dir.exists() {
                                discovered_symbols.insert(symbol.clone());
                                
                                if let Ok(date_entries) = std::fs::read_dir(&ticks_dir) {
                                    for date_entry in date_entries.flatten() {
                                        if let Some(file_name) = date_entry.file_name().to_str() {
                                            if file_name.ends_with(".parquet") {
                                                // Extract date from filename (e.g., "2025-11-25.parquet")
                                                if let Some(date) = file_name.strip_suffix(".parquet") {
                                                    // Update index if not already set
                                                    let current_status = availability_index
                                                        .check_availability(&symbol, date).await;
                                                    if current_status == DataAvailability::Unknown {
                                                        availability_index
                                                            .update_availability(&symbol, date, DataAvailability::Available)
                                                            .await;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            
                            // Check for kline subdirectories
                            if let Ok(kline_entries) = std::fs::read_dir(&symbol_dir) {
                                for kline_entry in kline_entries.flatten() {
                                    if kline_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                                        let timeframe = kline_entry.file_name().to_string_lossy().to_string();
                                        let kline_dir = kline_entry.path();
                                        
                                        if let Ok(date_entries) = std::fs::read_dir(&kline_dir) {
                                            discovered_symbols.insert(symbol.clone());
                                            
                                            for date_entry in date_entries.flatten() {
                                                if let Some(file_name) = date_entry.file_name().to_str() {
                                                    if file_name.ends_with(".parquet") {
                                                        if let Some(date) = file_name.strip_suffix(".parquet") {
                                                            let current_status = availability_index
                                                                .check_availability(&symbol, date).await;
                                                            if current_status == DataAvailability::Unknown {
                                                                availability_index
                                                                    .update_availability(&symbol, date, DataAvailability::Available)
                                                                    .await;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                
                log::info!("[HistoricalDataStatusWindow] Discovered {} files, {} unique symbols", discovered_count, discovered_symbols.len());
                
                // Get all symbols (including discovered ones)
                let mut symbols = availability_index.get_all_symbols().await;
                log::info!("[HistoricalDataStatusWindow] Index has {} symbols", symbols.len());
                for symbol in &discovered_symbols {
                    if !symbols.contains(symbol) {
                        symbols.push(symbol.clone());
                    }
                }
                log::info!("[HistoricalDataStatusWindow] Total symbols after merge: {}", symbols.len());
                
                let mut rows = Vec::new();
                for symbol in symbols {
                    let mut dates = availability_index.get_dates_for_symbol(&symbol).await;
                    
                    // If no dates found but symbol was discovered, add recent dates
                    if dates.is_empty() && discovered_symbols.contains(&symbol) {
                        // Add last 7 days
                        let today = chrono::Utc::now().date_naive();
                        for i in 0..7 {
                            let date = today - chrono::Duration::days(i);
                            let date_str = date.format("%Y-%m-%d").to_string();
                            dates.push(date_str);
                        }
                    }
                    
                    for date in dates {
                        let status = availability_index.check_availability(&symbol, &date).await;
                        let last_checked = availability_index.get_last_checked(&symbol, &date).await;
                        
                        // Get file size from cache
                        // Check flat structure first: cache/{symbol}_{date}_ticks.parquet
                        let file_size = {
                            let tick_path = cache_dir.join(format!("{}_{}_ticks.parquet", symbol, date));
                            if tick_path.exists() {
                                std::fs::metadata(&tick_path).ok().map(|m| m.len())
                            } else {
                                // Try kline files: cache/{symbol}_{date}_{timeframe}.parquet
                                let mut found_size = None;
                                for timeframe in &["1m", "5m", "15m", "1h", "4h", "1d"] {
                                    let kline_path = cache_dir.join(format!("{}_{}_{}.parquet", symbol, date, timeframe));
                                    if kline_path.exists() {
                                        found_size = std::fs::metadata(&kline_path).ok().map(|m| m.len());
                                        break;
                                    }
                                }
                                
                                // Also try nested structure (legacy)
                                if found_size.is_none() {
                                    let nested_tick_path = cache_dir.join(&symbol).join("ticks").join(format!("{}.parquet", date));
                                    if nested_tick_path.exists() {
                                        found_size = std::fs::metadata(&nested_tick_path).ok().map(|m| m.len());
                                    } else {
                                        for timeframe in &["1m", "5m", "15m", "1h", "4h", "1d"] {
                                            let nested_kline_path = cache_dir.join(&symbol).join("kline").join(timeframe).join(format!("{}.parquet", date));
                                            if nested_kline_path.exists() {
                                                found_size = std::fs::metadata(&nested_kline_path).ok().map(|m| m.len());
                                                break;
                                            }
                                        }
                                    }
                                }
                                
                                found_size
                            }
                        };
                        
                        rows.push(DataStatusRow {
                            symbol: symbol.clone(),
                            date,
                            status,
                            file_size,
                            last_checked,
                            download_progress: None,
                        });
                    }
                }
                
                log::info!("[HistoricalDataStatusWindow] Refresh complete. Total rows: {}", rows.len());
                rows
            },
            Message::RefreshComplete,
        )
    }

    /// Handles data events from the event bus.
    fn handle_data_event(&mut self, event: DataEvent) {
        match event {
            DataEvent::DownloadStarted { symbol, date, .. } => {
                self.update_row_status(&symbol, &date, DataAvailability::Downloading);
            }
            DataEvent::DownloadCompleted { symbol, date, .. } => {
                self.update_row_status(&symbol, &date, DataAvailability::Available);
            }
            DataEvent::DownloadFailed { symbol, date, .. } => {
                self.update_row_status(&symbol, &date, DataAvailability::Unavailable);
            }
            DataEvent::AvailabilityChanged { symbol, date, status } => {
                self.update_row_status(&symbol, &date, status);
            }
            _ => {}
        }
    }

    /// Updates the status of a row.
    fn update_row_status(&mut self, symbol: &str, date: &str, status: DataAvailability) {
        if let Some(row) = self.data_rows.iter_mut().find(|r| r.symbol == symbol && r.date == date) {
            row.status = status;
        }
    }

    /// Renders a table row.
    fn view_table_row<'a>(&self, row: &'a DataStatusRow) -> Element<'a, Message> {
        let status_text = match row.status {
            DataAvailability::Available => "✅ 可用",
            DataAvailability::Downloading => "⏳ 下载中",
            DataAvailability::Unavailable => "❌ 不可用",
            DataAvailability::Partial => "⚠️ 部分",
            DataAvailability::Unknown => "❓ 未知",
        };
        
        let size_text = row.file_size
            .map(|bytes| format!("{:.1} MB", bytes as f64 / 1_000_000.0))
            .unwrap_or_else(|| "-".to_string());
        
        let action_button = match row.status {
            DataAvailability::Available => {
                button("重下").on_press(Message::TriggerDownload {
                    symbol: row.symbol.clone(),
                    date: row.date.clone(),
                })
            }
            DataAvailability::Unavailable | DataAvailability::Unknown => {
                button("下载").on_press(Message::TriggerDownload {
                    symbol: row.symbol.clone(),
                    date: row.date.clone(),
                })
            }
            DataAvailability::Downloading => {
                button("取消").on_press(Message::TriggerDownload {
                    symbol: row.symbol.clone(),
                    date: row.date.clone(),
                })
            }
            DataAvailability::Partial => {
                button("修复").on_press(Message::TriggerDownload {
                    symbol: row.symbol.clone(),
                    date: row.date.clone(),
                })
            }
        };

        row![
            text(&row.symbol).width(Length::Fixed(100.0)),
            text(&row.date).width(Length::Fixed(120.0)),
            text(status_text).width(Length::Fixed(100.0)),
            text(size_text).width(Length::Fixed(100.0)),
            action_button.width(Length::Fixed(100.0)),
        ]
        .spacing(10)
        .padding(5)
        .into()
    }
}
