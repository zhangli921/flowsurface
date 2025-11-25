#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod chart;
mod layout;
mod logger;
mod modal;
mod screen;
mod style;
mod widget;
mod window;

use data::{
    self,
    unified_data_service::UnifiedDataService,
    realtime_ingester::{RealtimeIngesterService, IngestCommand, normalize_binance_symbol},
    compute::service::VpComputeService,
    config::theme::default_theme,
    realtime_data_service::{RealtimeDataService, TimeRange},
    kline::KLine,
    kline_cache::KlineCache,
    layout::WindowSpec,
    sidebar, DataError,
    data_availability_index::DataAvailabilityIndex,
    historical_data_service::HistoricalDataService,
    historical_download_coordinator::HistoricalDownloadCoordinator,
    historical_download_executor::HistoricalDownloadExecutor,
};
use layout::{configuration, Layout};
use modal::{audio, dashboard_modal, main_dialog_modal, LayoutManager, ThemeEditor};
use screen::dashboard::{self, Dashboard};
use storage::MmapStore; // Now explicitly imported
use widget::{
    confirm_dialog_container, // Now explicitly imported
    toast::{self, Manager as ToastManager, Toast}, // Now explicitly imported
    tooltip,                  // Now explicitly imported
};

use iced::{
    Alignment, Element, Subscription, Task, keyboard, padding,
    widget::{
        button, column, container, pane_grid, pick_list, row, rule, scrollable, text, // scrollable added
        tooltip::Position as TooltipPosition,
    },
};
use std::{
    borrow::Cow,
    collections::HashMap,
    env,
    path::PathBuf,
    sync::Arc,
    vec,
};
use tokio::sync::broadcast;

fn main() {
    logger::setup(cfg!(debug_assertions)).expect("Failed to initialize logger");

    std::thread::spawn(data::cleanup_old_market_data);

    let _ = iced::daemon(Flowsurface::new, Flowsurface::update, Flowsurface::view)
        .settings(iced::Settings {
            antialiasing: true,
            fonts: vec![
                Cow::Borrowed(style::AZERET_MONO_BYTES),
                Cow::Borrowed(style::ICONS_BYTES),
            ],
            default_text_size: iced::Pixels(12.0),
            ..Default::default()
        })
        .title(Flowsurface::title)
        .theme(Flowsurface::theme)
        .scale_factor(Flowsurface::scale_factor)
        .subscription(Flowsurface::subscription)
        .run();
}

struct Flowsurface {
    main_window: window::Window,
    sidebar: dashboard::Sidebar,
    unified_data_service: Arc<UnifiedDataService>,
    vp_service: Option<Arc<VpComputeService>>,
    layout_manager: LayoutManager,
    theme_editor: ThemeEditor,
    audio_stream: audio::AudioStream,
    confirm_dialog: Option<(String, Box<Message>)>,
    preferred_currency: exchange::PreferredCurrency,
    scale_factor: data::ScaleFactor,
    timezone: data::UserTimezone,
    theme: data::Theme,
    notifications: Vec<Toast>,
    ingest_tx: tokio::sync::mpsc::Sender<IngestCommand>,
    event_bus: Arc<data::EventBus>, // Unified event bus for data services
    availability_index: Arc<DataAvailabilityIndex>,
    download_coordinator: Arc<HistoricalDownloadCoordinator>,
    historical_data_status_window: Option<(window::Id, screen::historical_data_status::HistoricalDataStatusWindow)>,
    // VP overlap threshold tracking
    vp_overlap_threshold: Option<(String, f64)>, // (symbol, overlap_ratio)
    // EventBus receiver for polling events (wrapped in Arc<Mutex> to be Send + Sync)
    event_bus_receiver: Arc<tokio::sync::Mutex<tokio::sync::broadcast::Receiver<data::DataEvent>>>,
}

#[derive(Debug, Clone)]
enum Message {
    Sidebar(dashboard::sidebar::Message),
    MarketWsEvent(exchange::Event),
    Dashboard(Option<uuid::Uuid>, dashboard::Message),
    FetchKLines(String, TimeRange, String), // symbol, time range, timeframe
    KLineDataFetched(Result<Vec<KLine>, Arc<DataError>>),
    ComputeVp(String, TimeRange), // symbol, time range
    VpComputed(String, Result<data::compute::vp::VolumeProfile, data::compute::vp::ComputeError>), // (symbol, result)
    VpServiceInitialized(Result<Arc<VpComputeService>, String>),
    Tick(std::time::Instant),
    WindowEvent(window::Event),
    ExitRequested(HashMap<window::Id, WindowSpec>),
    GoBack,
    DataFolderRequested,
    ThemeSelected(data::Theme),
    ScaleFactorChanged(data::ScaleFactor),
    SetTimezone(data::UserTimezone),
    ToggleTradeFetch(bool),
    ToggleShowQuoteCurrency(bool),
    RemoveNotification(usize),
    ToggleDialogModal(Option<(String, Box<Message>)>),
    ThemeEditor(modal::theme_editor::Message),
    Layouts(modal::layout_manager::Message),
    AudioStream(modal::audio::Message),
    DataEvent(data::DataEvent), // Event from data service event bus
    OpenHistoricalDataStatusWindow,
    HistoricalDataStatus(screen::historical_data_status::Message),
}

impl Flowsurface {
    fn new() -> (Self, Task<Message>) {
        // --- Start Ingestion Service ---
        let (ingest_tx, ingest_rx) = tokio::sync::mpsc::channel(100);
        let data_dir = data::data_path(Some("market_data"));
        
        // Pre-create the default Mmap file synchronously before starting the async service
        RealtimeIngesterService::ensure_mmap_file("BTCUSDT", &data_dir);

        // Create shared K-line cache
        let kline_cache = Arc::new(KlineCache::new());
        
        // Initialize RealtimeIngesterService with K-line cache
        let mut ingestion_service = RealtimeIngesterService::new(ingest_rx, data_dir);
        ingestion_service.set_kline_cache(kline_cache.clone());
        tokio::spawn(ingestion_service.run());
        // -------------------------------

        // --- Start of Unified Data Service Initialization ---
        // Create unified event bus for data services
        let event_bus = Arc::new(data::EventBus::new());
        
        // Create data availability index
        let availability_index = Arc::new(data::DataAvailabilityIndex::new());
        
        // Create historical download executor
        let download_executor = Arc::new(HistoricalDownloadExecutor::new(None));
        
        // Create historical download coordinator (and spawn download loop)
        let download_coordinator = Arc::new(HistoricalDownloadCoordinator::new(
            availability_index.clone(),
            download_executor.clone(),
            event_bus.clone(),
        ));
        
        // Spawn download loop as background task
        let download_coordinator_clone = download_coordinator.clone();
        tokio::spawn(async move {
            download_coordinator_clone.run_download_loop().await;
        });
        
        let data_dir = data::data_path(Some("market_data"));
        std::fs::create_dir_all(&data_dir).ok();

        let mut realtime_data_service = RealtimeDataService::new(data_dir.clone());
        realtime_data_service.set_kline_cache(kline_cache.clone());
        
        // Create historical data service with new dependencies
        let historical_data_service = Arc::new(HistoricalDataService::new(
            download_executor.clone(),
            availability_index.clone(),
            download_coordinator.clone(),
        ));
        
        let unified_data_service = Arc::new(UnifiedDataService::new(
            Arc::new(realtime_data_service),
            historical_data_service,
        ));
        // --- End of Unified Data Service Initialization ---

        let saved_state = layout::load_saved_state(unified_data_service.clone());

        let (main_window_id, open_main_window) = {
            let (position, size) = saved_state.window();
            let config = window::Settings {
                size,
                position,
                exit_on_close_request: false,
                ..window::settings()
            };
            window::open(config)
        };

        let (sidebar, launch_sidebar) = dashboard::Sidebar::new(&saved_state);

        // Initialize VpComputeService asynchronously
        let init_vp_service = Task::perform(
            async move {
                VpComputeService::new()
                    .await
                    .map(Arc::new)
            },
            Message::VpServiceInitialized,
        );

        let mut state = Self {
            main_window: window::Window::new(main_window_id),
            unified_data_service: unified_data_service.clone(),
            vp_service: None, // Will be initialized asynchronously
            layout_manager: saved_state.layout_manager,
            theme_editor: ThemeEditor::new(saved_state.custom_theme),
            audio_stream: audio::AudioStream::new(saved_state.audio_cfg),
            event_bus: event_bus.clone(),
            availability_index: availability_index.clone(),
            download_coordinator: download_coordinator.clone(),
            historical_data_status_window: None,
            sidebar,
            confirm_dialog: None,
            timezone: saved_state.timezone,
            scale_factor: saved_state.scale_factor,
            preferred_currency: saved_state.preferred_currency,
            theme: saved_state.theme,
            notifications: vec![],
            ingest_tx,
            vp_overlap_threshold: None,
            event_bus_receiver: Arc::new(tokio::sync::Mutex::new(event_bus.subscribe())),
        };
        
        // Update all dashboards with the unified_data_service
        for dashboard in state.layout_manager.iter_dashboards_mut() {
            dashboard.set_unified_data_service(unified_data_service.clone());
        }

        let last_active_layout = state.layout_manager.active_layout();
        let load_layout = state.load_layout(last_active_layout, main_window_id);

        (
            state,
            open_main_window
                .discard()
                .chain(load_layout)
                .chain(launch_sidebar.map(Message::Sidebar))
                .chain(init_vp_service),
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::MarketWsEvent(event) => {
                let main_window_id = self.main_window.id;
                let dashboard = self.active_dashboard_mut();

                match event {
                    exchange::Event::Connected(_exchange) => {
                        // Stream connected
                    }
                    exchange::Event::Disconnected(_exchange, _reason) => {
                        // Stream disconnected
                    }
                    exchange::Event::DepthReceived(
                        stream,
                        depth_update_t,
                        depth,
                        trades_buffer,
                    ) => {
                        let task = dashboard
                            .update_depth_and_trades(
                                &stream,
                                depth_update_t,
                                &depth,
                                &trades_buffer,
                                main_window_id,
                            )
                            .map(move |msg| Message::Dashboard(None, msg));

                        if let Err(err) = self.audio_stream.try_play_sound(&stream, &trades_buffer)
                        {
                            log::error!("Failed to play sound: {err}");
                        }

                        return task;
                    }
                    exchange::Event::KlineReceived(stream, kline) => {
                        return dashboard
                            .update_latest_klines(&stream, &kline, main_window_id)
                            .map(move |msg| Message::Dashboard(None, msg));
                    }
                }
            }
            Message::FetchKLines(symbol, range, timeframe) => {
                let unified_service = self.unified_data_service.clone();
                return Task::perform(
                    async move {
                        unified_service
                            .fetch_klines(symbol, range, &timeframe)
                            .await
                            .map_err(Arc::new)
                    },
                    Message::KLineDataFetched,
                );
            }
            Message::KLineDataFetched(result) => {
                match result {
                    Ok(_klines) => {
                        // K-lines fetched successfully
                    }
                    Err(e) => {
                        log::error!("Failed to fetch klines: {}", e);
                    }
                }
            }
            Message::VpServiceInitialized(result) => {
                match result {
                    Ok(service) => {
                        self.vp_service = Some(service);
                    }
                    Err(e) => {
                        log::error!("Failed to initialize VpComputeService: {}", e);
                        self.notifications.push(Toast::error(format!(
                            "GPU Compute Init Failed: {}",
                            e
                        )));
                    }
                }
            }
            Message::ComputeVp(symbol, range) => {
                if let Some(service) = &self.vp_service {
                    let service = service.clone();
                    let unified_service = self.unified_data_service.clone();
                    let symbol_clone = symbol.clone();
                    let event_bus = self.event_bus.clone();
                    
                    // Trigger ingestion if needed (for real-time data)
                    let normalized_symbol = normalize_binance_symbol(&symbol_clone);
                    let data_dir = data::data_path(Some("market_data"));
                    let mmap_path = {
                        let mut path = data_dir.clone();
                        std::fs::create_dir_all(&path).ok();
                        path.push(format!("{}.mmap", normalized_symbol));
                        path
                    };
                    
                    // Check if real-time data file exists, if not, trigger Ingester
                    let file_ready = mmap_path.exists() && {
                        if let Ok(metadata) = std::fs::metadata(&mmap_path) {
                            metadata.len() > 1024
                        } else {
                            false
                        }
                    };
                    
                    // Try to get timeframe from the chart
                    let timeframe_opt = {
                        let dashboard = self.active_dashboard_mut();
                        dashboard.find_pane_by_symbol(&symbol_clone)
                            .and_then(|pane| {
                                if let screen::dashboard::pane::Content::Kline { chart: Some(chart), .. } = &pane.content {
                                    // Use basis() method to get timeframe
                                    match chart.basis() {
                                        data::chart::Basis::Time(tf) => Some(tf.to_string()),
                                        _ => None,
                                    }
                                } else {
                                    None
                                }
                            })
                    };
                    
                    if !file_ready {
                        let _ = self.ingest_tx.try_send(IngestCommand::Subscribe(symbol_clone.clone(), timeframe_opt));
                    }
                    
                    // Publish VP computation started event
                    let _ = event_bus.publish(data::DataEvent::VpComputeStarted {
                        symbol: symbol_clone.clone(),
                        range_start_us: range.start_us,
                        range_end_us: range.end_us,
                    });
                    
                    return Task::future(async move {
                        // Use UnifiedDataService to fetch ticks (automatically handles real-time and historical)
                        let ticks = match unified_service.fetch_ticks(symbol_clone.clone(), range).await {
                            Ok(ticks) => ticks,
                            Err(e) => {
                                let compute_err: data::compute::vp::ComputeError = e.into();
                                // Publish failure event
                                let _ = event_bus.publish(data::DataEvent::VpComputeFailed {
                                    symbol: symbol_clone.clone(),
                                    range_start_us: range.start_us,
                                    range_end_us: range.end_us,
                                    error: compute_err.to_string(),
                                });
                                return Message::VpComputed(symbol_clone.clone(), Err(compute_err));
                            },
                        };
                        
                        // Calculate overlap ratio and publish event
                        let overlap_ratio = if let Some((actual_start, actual_end)) = ticks.time_range {
                            let overlap_start = actual_start.max(range.start_us);
                            let overlap_end = actual_end.min(range.end_us);
                            
                            if overlap_start >= overlap_end {
                                0.0
                            } else {
                                let requested_span = range.end_us.saturating_sub(range.start_us);
                                let overlap_span = overlap_end.saturating_sub(overlap_start);
                                
                                if requested_span > 0 {
                                    overlap_span as f64 / requested_span as f64
                                } else {
                                    1.0
                                }
                            }
                        } else {
                            1.0 // Old format
                        };
                        
                        // Publish overlap threshold updated event
                        let _ = event_bus.publish(data::DataEvent::VpOverlapThresholdUpdated {
                            symbol: symbol_clone.clone(),
                            overlap_ratio,
                        });
                        
                        // CRITICAL: Verify data completeness before computing VP
                        // Check if the tick data covers the requested time range
                        let data_complete = if let Some((actual_start, actual_end)) = ticks.time_range {
                            // Calculate the overlap between requested and actual ranges
                            let overlap_start = actual_start.max(range.start_us);
                            let overlap_end = actual_end.min(range.end_us);
                            
                            // Check if there's meaningful overlap
                            if overlap_start >= overlap_end {
                                // No overlap at all
                                false
                            } else {
                                let requested_span = range.end_us.saturating_sub(range.start_us);
                                let overlap_span = overlap_end.saturating_sub(overlap_start);
                                
                                // For historical data, we're more lenient:
                                // - If overlap covers at least 50% of requested range, proceed
                                // - OR if the actual data range is large enough (>= 12 hours), proceed
                                //   (this handles cases where some dates are missing but we have substantial data)
                                let overlap_ratio = if requested_span > 0 {
                                    overlap_span as f64 / requested_span as f64
                                } else {
                                    1.0
                                };
                                
                                let actual_span = actual_end.saturating_sub(actual_start);
                                let has_substantial_data = actual_span >= 12 * 3_600_000_000; // 12 hours in microseconds
                                
                                overlap_ratio >= 0.5 || has_substantial_data
                            }
                        } else {
                            // If time_range is None (old cache files or data without timestamps), we can't verify completeness
                            // Proceed with computation (old data format is still valid)
                            true
                        };
                        
                        if !data_complete {
                            log::warn!(
                                "VP computation skipped for {}: Data incomplete. Requested: {} - {} us, Actual: {:?}",
                                symbol_clone, range.start_us, range.end_us, ticks.time_range
                            );
                            // Publish failure event
                            let _ = event_bus.publish(data::DataEvent::VpComputeFailed {
                                symbol: symbol_clone.clone(),
                                range_start_us: range.start_us,
                                range_end_us: range.end_us,
                                error: format!("Tick data incomplete for range {} - {} us", range.start_us, range.end_us),
                            });
                            return Message::VpComputed(symbol_clone.clone(), Err(
                                data::compute::vp::ComputeError::Other(
                                    format!("Tick data incomplete for range {} - {} us", range.start_us, range.end_us)
                                )
                            ));
                        }
                        
                        // Calculate compute parameters
                        let num_ticks = ticks.prices.len() as u32;
                        if num_ticks == 0 {
                            // Publish failure event
                            let _ = event_bus.publish(data::DataEvent::VpComputeFailed {
                                symbol: symbol_clone.clone(),
                                range_start_us: range.start_us,
                                range_end_us: range.end_us,
                                error: "No ticks found".to_string(),
                            });
                            return Message::VpComputed(symbol_clone.clone(), Err(
                                data::compute::vp::ComputeError::Other("No ticks found".to_string())
                            ));
                        }
                        
                        let min_price = *ticks.prices.iter().min().unwrap_or(&0) as u32;
                        let max_price = *ticks.prices.iter().max().unwrap_or(&0) as u32;
                        let price_range = max_price.saturating_sub(min_price);
                        let price_resolution = 1; // 1 cent resolution
                        let histogram_buckets = (price_range / price_resolution).max(1) as u64;
                        
                        let params = data::compute::vp::ComputeParams {
                            num_ticks,
                            price_resolution,
                            min_price,
                            volume_scaling_factor: 10000, // Scale by 10000 to preserve 4 decimal places
                            tick_offset: 0, // Start from beginning
                        };
                        
                        // Run compute on GPU
                        let result = service.compute_vp(&ticks, &params, histogram_buckets).await;
                        
                        // Publish completion event
                        match &result {
                            Ok(profile) => {
                                let _ = event_bus.publish(data::DataEvent::VpComputeCompleted {
                                    symbol: symbol_clone.clone(),
                                    range_start_us: range.start_us,
                                    range_end_us: range.end_us,
                                    bar_count: profile.bars.len(),
                                });
                            }
                            Err(e) => {
                                let _ = event_bus.publish(data::DataEvent::VpComputeFailed {
                                    symbol: symbol_clone.clone(),
                                    range_start_us: range.start_us,
                                    range_end_us: range.end_us,
                                    error: e.to_string(),
                                });
                            }
                        }
                        
                        Message::VpComputed(symbol_clone, result)
                    });
                } else {
                    log::warn!("ComputeVp requested but VpComputeService is not ready.");
                }
            }
            Message::VpComputed(symbol, result) => {
                match result {
                    Ok(profile) => {
                        log::debug!(
                            "[VP] Computation successful for {}: {} bars, POC: {}, VA: {} - {}",
                            symbol,
                            profile.bars.len(),
                            profile.point_of_control,
                            profile.value_area_start,
                            profile.value_area_end
                        );
                        // Find the chart for this symbol and update its VP data
                        let dashboard = self.active_dashboard_mut();
                        
                        if let Some(pane) = dashboard.find_pane_by_symbol(&symbol) {
                            if let screen::dashboard::pane::Content::Kline { chart: Some(chart), .. } = &mut pane.content {
                                chart.set_volume_profile(profile);
                            } else {
                                log::warn!("[VP] Chart not found or not a Kline chart for {}", symbol);
                            }
                        } else {
                            log::warn!("[VP] Pane not found for {}", symbol);
                        }
                    }
                    Err(e) => {
                        // VP computation failed - check if it's due to incomplete data
                        let error_msg = e.to_string();
                        let is_data_incomplete = error_msg.contains("incomplete") || error_msg.contains("No ticks found");
                        
                        if is_data_incomplete {
                            // Mark VP as needing update so it will retry when data is available
                            let dashboard = self.active_dashboard_mut();
                            if let Some(pane) = dashboard.find_pane_by_symbol(&symbol) {
                                if let screen::dashboard::pane::Content::Kline { chart: Some(chart), .. } = &mut pane.content {
                                    // Re-enable VP update flag so it will retry
                                    chart.mark_vp_needs_update();
                                }
                            }
                        } else {
                            // Other errors (GPU, computation, etc.) - log but don't retry
                            log::warn!("VP computation failed for {}: {}", symbol, e);
                        }
                    }
                }
            }
            Message::Tick(now) => {
                let main_window_id = self.main_window.id;
                
                // Poll EventBus for new events (non-blocking)
                let receiver = self.event_bus_receiver.clone();
                let poll_task = Task::perform(
                    async move {
                        let mut receiver = receiver.lock().await;
                        let mut events = Vec::new();
                        // Collect up to 10 events per tick to avoid blocking
                        for _ in 0..10 {
                            match receiver.try_recv() {
                                Ok(event) => events.push(event),
                                Err(broadcast::error::TryRecvError::Empty) => break,
                                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                                    log::warn!("EventBus subscription lagged, skipped {} events", skipped);
                                    break; // Stop after lag to avoid infinite loop
                                }
                                Err(broadcast::error::TryRecvError::Closed) => break,
                            }
                        }
                        events
                    },
                    move |events| {
                        if events.is_empty() {
                            Message::Tick(now)
                        } else {
                            // Process all events by returning the first one
                            // Remaining events will be processed in subsequent ticks
                            // This is acceptable since events are not time-critical
                            Message::DataEvent(events.into_iter().next().unwrap())
                        }
                    },
                );
                
                let dashboard_tick = self.active_dashboard_mut().tick(now, main_window_id)
                    .map(move |msg| Message::Dashboard(None, msg));
                
                return poll_task.chain(dashboard_tick);
            }
            Message::WindowEvent(event) => match event {
                window::Event::CloseRequested(window) => {
                    let main_window = self.main_window.id;

                    if window != main_window {
                        // Check if it's the historical data status window
                        let is_historical_window = self.historical_data_status_window
                            .as_ref()
                            .map_or(false, |(window_id, _)| *window_id == window);
                        
                        if is_historical_window {
                            self.historical_data_status_window = None;
                            return window::close(window);
                        }
                        
                        // Otherwise, it's a popout window
                        let dashboard = self.active_dashboard_mut();
                        dashboard.popout.remove(&window);
                        return window::close(window);
                    }

                    let dashboard = self.active_dashboard_mut();
                    let mut opened_windows = dashboard
                        .popout
                        .keys()
                        .copied()
                        .collect::<Vec<window::Id>>();

                    opened_windows.push(main_window);

                    return window::collect_window_specs(opened_windows, Message::ExitRequested);
                }
            },
            Message::ExitRequested(windows) => {
                self.active_dashboard_mut()
                    .popout
                    .iter_mut()
                    .for_each(|(id, (_, window_spec))| {
                        if let Some(new_window_spec) = windows.get(id) {
                            *window_spec = *new_window_spec;
                        }
                    });

                let mut ser_layouts = vec![];

                for id in &self.layout_manager.layout_order {
                    if let Some((layout, dashboard)) = self.layout_manager.get_layout(*id) {
                        let serialized_dashboard = data::Dashboard::from(dashboard);

                        ser_layouts.push(data::Layout {
                            name: layout.name.clone(),
                            dashboard: serialized_dashboard,
                        });
                    }
                }

                let layouts = data::Layouts {
                    layouts: ser_layouts,
                    active_layout: self.layout_manager.active_layout().name.clone(),
                };

                let main_window = windows
                    .iter()
                    .find(|(id, _)| **id == self.main_window.id)
                    .map(|(_, spec)| *spec);

                let audio_cfg = data::AudioStream::from(&self.audio_stream);

                self.sidebar.sync_tickers_table_settings();

                let layout = data::State::from_parts(
                    layouts,
                    self.theme.clone(),
                    self.theme_editor.custom_theme.clone().map(data::Theme),
                    main_window,
                    self.timezone,
                    self.sidebar.state.clone(),
                    self.scale_factor,
                    audio_cfg,
                    self.preferred_currency,
                );

                match serde_json::to_string(&layout) {
                    Ok(layout_str) => {
                        let file_name = data::SAVED_STATE_PATH;

                        if let Err(e) = data::write_json_to_file(&layout_str, file_name) {
                            log::error!("Failed to write layout state to file: {}", e);
                        } else {
                            log::info!("Successfully wrote layout state to {file_name}");
                        }
                    }
                    Err(e) => log::error!("Failed to serialize layout: {}", e),
                }

                return iced::exit();
            }
            Message::GoBack => {
                let main_window = self.main_window.id;

                if self.confirm_dialog.is_some() {
                    self.confirm_dialog = None;
                } else if self.sidebar.active_menu().is_some() {
                    self.sidebar.set_menu(None);
                } else {
                    let dashboard = self.active_dashboard_mut();

                    if dashboard.go_back(main_window) {
                        return Task::none();
                    } else if dashboard.focus.is_some() {
                        dashboard.focus = None;
                    } else {
                        self.sidebar.hide_tickers_table();
                    }
                }
            }
            Message::ThemeSelected(theme) => {
                self.theme = theme.clone();
            }
            Message::Dashboard(id, message) => {
                // Handle ComputeVp specially - forward to main app's ComputeVp handler
                if let dashboard::Message::ComputeVp(symbol, range) = &message {
                    return self.update(Message::ComputeVp(symbol.clone(), *range));
                }
                
                let main_window = self.main_window;
                let layout_id = id.unwrap_or(self.layout_manager.active_layout().id);

                if let Some(dashboard) = self.layout_manager.mut_dashboard(&layout_id) {
                    let (main_task, event) = dashboard.update(message, &main_window, &layout_id);

                    let additional_task = match event {
                        Some(dashboard::Event::DistributeFetchedData {
                            layout_id,
                            pane_id,
                            data,
                            stream,
                        }) => dashboard
                            .distribute_fetched_data(main_window.id, pane_id, data, stream)
                            .map(move |msg| Message::Dashboard(Some(layout_id), msg)),
                        Some(dashboard::Event::Notification(toast)) => {
                            self.notifications.push(toast);
                            Task::none()
                        }
                        Some(dashboard::Event::ResolveStreams { pane_id, streams, timeframe }) => {
                            // Notify Ingestion Service and ensure mmap file exists
                            if let Some(stream) = streams.first() {
                                let symbol = match stream {
                                    exchange::adapter::PersistStreamKind::Kline(pk) => pk.ticker.to_string(),
                                    exchange::adapter::PersistStreamKind::DepthAndTrades(pd) => pd.ticker.to_string(),
                                };
                                if !symbol.is_empty() {
                                    // Check if mmap file exists, if not trigger Ingester
                                    let normalized_symbol = normalize_binance_symbol(&symbol);
                                    let data_dir = data::data_path(Some("market_data"));
                                    let mmap_path = data_dir.join(format!("{}.mmap", normalized_symbol));
                                    
                                    let file_ready = mmap_path.exists() && {
                                        if let Ok(metadata) = std::fs::metadata(&mmap_path) {
                                            metadata.len() > 1024
                                        } else {
                                            false
                                        }
                                    };
                                    
                                    if !file_ready {
                                        // Mmap file doesn't exist, trigger Ingester
                                    }
                                    
                                    match self.ingest_tx.try_send(IngestCommand::Subscribe(symbol.clone(), timeframe)) {
                                        Ok(()) => {},
                                        Err(e) => log::warn!("Failed to send IngestCommand::Subscribe({}): {:?}", symbol, e),
                                    }
                                } else {
                                    log::warn!("Empty symbol extracted from stream, skipping IngestCommand::Subscribe");
                                }
                            } else {
                                log::warn!("ResolveStreams event received but streams is empty");
                            }

                            let tickers_info = self.sidebar.tickers_info();

                            let resolved_streams =
                                streams.into_iter().try_fold(vec![], |mut acc, persist| {
                                    let resolver = |t: &exchange::Ticker| {
                                        tickers_info.get(t).and_then(|opt| *opt)
                                    };

                                    match persist.into_stream_kind(resolver) {
                                        Ok(stream) => {
                                            acc.push(stream);
                                            Ok(acc)
                                        }
                                        Err(err) => Err(format!(
                                            "Failed to resolve persisted stream: {}",
                                            err
                                        )),
                                    }
                                });

                            match resolved_streams {
                                Ok(resolved) => {
                                    if resolved.is_empty() {
                                        Task::none()
                                    } else {
                                        dashboard
                                            .resolve_streams(main_window.id, pane_id, resolved)
                                            .map(move |msg| Message::Dashboard(None, msg))
                                    }
                                }
                                Err(_err) => {
                                    // Failed to resolve persisted stream (e.g., TickerInfo not found) - silently ignore
                                    Task::none()
                                }
                            }
                        }
                        None => Task::none(),
                    };

                    return main_task
                        .map(move |msg| Message::Dashboard(Some(layout_id), msg))
                        .chain(additional_task);
                }
            }
            Message::RemoveNotification(index) => {
                if index < self.notifications.len() {
                    self.notifications.remove(index);
                }
            }
            Message::SetTimezone(tz) => {
                self.timezone = tz;
            }
            Message::ScaleFactorChanged(value) => {
                self.scale_factor = value;
            }
            Message::ToggleTradeFetch(checked) => {
                self.layout_manager
                    .iter_dashboards_mut()
                    .for_each(|dashboard| {
                        dashboard.toggle_trade_fetch(checked, &self.main_window);
                    });

                if checked {
                    self.confirm_dialog = None;
                }
            }
            Message::ToggleShowQuoteCurrency(checked) => {
                self.preferred_currency = if checked {
                    exchange::PreferredCurrency::Quote
                } else {
                    exchange::PreferredCurrency::Base
                };

                if self.confirm_dialog.is_some() {
                    self.confirm_dialog = None;
                }
            }
            Message::ToggleDialogModal(dialog) => {
                self.confirm_dialog = dialog;
            }
            Message::Layouts(message) => {
                let action = self.layout_manager.update(message);

                match action {
                    Some(modal::layout_manager::Action::Select(layout)) => {
                        let old_layout = self.layout_manager.active_layout().clone();

                        let active_popout_keys = self
                            .active_dashboard()
                            .popout
                            .keys()
                            .copied()
                            .collect::<Vec<_>>();

                        let window_tasks = Task::batch(
                            active_popout_keys
                                .iter()
                                .map(|&popout_id| window::close(popout_id))
                                .collect::<Vec<_>>(),
                        )
                        .then(|_: Task<window::Id>| Task::none());

                        return window::collect_window_specs(
                            active_popout_keys,
                            dashboard::Message::SavePopoutSpecs,
                        )
                        .map(move |msg| Message::Dashboard(Some(old_layout.id), msg))
                        .chain(window_tasks)
                        .chain(self.load_layout(layout, self.main_window.id));
                    }
                    Some(modal::layout_manager::Action::Clone(id)) => {
                        let manager = &mut self.layout_manager;

                        if let Some((layout, dashboard)) = manager.get_layout(id) {
                            let new_id = uuid::Uuid::new_v4();
                            let new_layout = Layout {
                                id: new_id,
                                name: manager.ensure_unique_name(&layout.name, new_id),
                            };

                            let ser_dashboard = data::Dashboard::from(dashboard);

                            let mut popout_windows = Vec::new();

                            for (pane, window_spec) in &ser_dashboard.popout {
                                let configuration = configuration(pane.clone());
                                popout_windows.push((configuration, *window_spec));
                            }

                            let dashboard = Dashboard::from_config(
                                configuration(ser_dashboard.pane.clone()),
                                popout_windows,
                                layout.id,
                                self.unified_data_service.clone(),
                            );

                            manager.layout_order.push(new_layout.id);
                            manager
                                .layouts
                                .insert(new_layout.id, (new_layout.clone(), dashboard));
                        }
                    }
                    None => {}
                }
            }
            Message::AudioStream(message) => self.audio_stream.update(message),
            Message::DataEvent(event) => {
                // Handle VP-related events
                match &event {
                    data::DataEvent::VpOverlapThresholdUpdated { symbol, overlap_ratio } => {
                        // Update overlap threshold for display
                        self.vp_overlap_threshold = Some((symbol.clone(), *overlap_ratio));
                    }
                    data::DataEvent::VpComputeStarted { symbol, .. } => {
                        log::debug!("VP computation started for {}", symbol);
                    }
                    data::DataEvent::VpComputeCompleted { symbol, bar_count, .. } => {
                        log::debug!("VP computation completed for {}: {} bars", symbol, bar_count);
                    }
                    data::DataEvent::VpComputeFailed { symbol, error, .. } => {
                        log::warn!("VP computation failed for {}: {}", symbol, error);
                    }
                    _ => {}
                }
                
                // Forward events to historical data status window if open
                if let Some((_, ref mut window)) = self.historical_data_status_window {
                    return window.update(screen::historical_data_status::Message::DataEvent(event))
                        .map(Message::HistoricalDataStatus);
                }
                
                // Log other events if window is not open
                match event {
                    data::DataEvent::DownloadStarted { symbol, date, .. } => {
                        log::debug!("Download started: {} {}", symbol, date);
                    }
                    data::DataEvent::DownloadCompleted { symbol, date, .. } => {
                        log::debug!("Download completed: {} {}", symbol, date);
                    }
                    data::DataEvent::DownloadFailed { symbol, date, error, .. } => {
                        log::warn!("Download failed: {} {} - {}", symbol, date, error);
                    }
                    data::DataEvent::AvailabilityChanged { symbol, date, status } => {
                        log::debug!("Availability changed: {} {} - {:?}", symbol, date, status);
                    }
                    _ => {}
                }
            }
            Message::OpenHistoricalDataStatusWindow => {
                if self.historical_data_status_window.is_none() {
                    let (mut window_state, _predefined_window_id) = screen::historical_data_status::HistoricalDataStatusWindow::new(
                        self.availability_index.clone(),
                        self.download_coordinator.clone(),
                        self.event_bus.clone(),
                    );
                    
                    // Trigger initial data refresh
                    let refresh_task = window_state.update(screen::historical_data_status::Message::Refresh);
                    
                    let window_config = window::Settings {
                        size: iced::Size::new(1000.0, 700.0),
                        exit_on_close_request: false,
                        ..window::settings()
                    };
                    
                    let (actual_window_id, open_task) = window::open(window_config);
                    self.historical_data_status_window = Some((actual_window_id, window_state));
                    
                    return open_task
                        .map(|_| Message::Tick(std::time::Instant::now()))
                        .chain(refresh_task.map(Message::HistoricalDataStatus));
                }
            }
            Message::HistoricalDataStatus(msg) => {
                if let Some((_, ref mut window)) = self.historical_data_status_window {
                    return window.update(msg).map(Message::HistoricalDataStatus);
                }
            }
            Message::DataFolderRequested => {
                if let Err(err) = data::open_data_folder() {
                    self.notifications
                        .push(Toast::error(format!("Failed to open data folder: {err}")));
                }
            }
            Message::ThemeEditor(msg) => {
                let action = self.theme_editor.update(msg, &self.theme.clone().into());

                match action {
                    Some(modal::theme_editor::Action::Exit) => {
                        self.sidebar.set_menu(Some(sidebar::Menu::Settings));
                    }
                    Some(modal::theme_editor::Action::UpdateTheme(theme)) => {
                        self.theme = data::Theme(theme);

                        let main_window = self.main_window.id;

                        self.active_dashboard_mut()
                            .invalidate_all_panes(main_window);
                    }
                    None => {}
                }
            }
            Message::Sidebar(message) => {
                let (task, action) = self.sidebar.update(message);

                match action {
                    Some(dashboard::sidebar::Action::TickerSelected(ticker_info, content)) => {
                        let main_window_id = self.main_window.id;

                        let task = {
                            if let Some(kind) = content {
                                self.active_dashboard_mut().init_focused_pane(
                                    main_window_id,
                                    ticker_info,
                                    kind,
                                )
                            } else {
                                self.active_dashboard_mut()
                                    .switch_tickers_in_group(main_window_id, ticker_info)
                            }
                        };

                        return task.map(move |msg| Message::Dashboard(None, msg));
                    }
                    Some(dashboard::sidebar::Action::ErrorOccurred(err)) => {
                        self.notifications.push(Toast::error(err.to_string()));
                    }
                    None => {}
                }

                return task.map(Message::Sidebar);
            }
        }
        Task::none()
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        let dashboard = self.active_dashboard();
        let sidebar_pos = self.sidebar.position();

        let tickers_table = &self.sidebar.tickers_table;

        let content = if id == self.main_window.id {
            let sidebar_view = self
                .sidebar
                .view(self.audio_stream.volume())
                .map(Message::Sidebar);

            let dashboard_view = dashboard
                .view(&self.main_window, tickers_table, self.timezone)
                .map(move |msg| Message::Dashboard(None, msg));

            let header_title = {
                #[cfg(target_os = "macos")]
                {
                    let overlap_info = if let Some((symbol, ratio)) = &self.vp_overlap_threshold {
                        row![
                            text(format!("VP重叠阈值: {} ({:.1}%)", symbol, ratio * 100.0))
                                .size(12)
                                .style(style::title_text),
                        ]
                        .spacing(8)
                    } else {
                        row![]
                    };
                    
                    iced::widget::center(
                        column![
                            text("FLOWSURFACE")
                                .font(iced::Font {
                                    weight: iced::font::Weight::Bold,
                                    ..Default::default()
                                })
                                .size(16)
                                .style(style::title_text),
                            overlap_info,
                        ]
                        .spacing(4)
                    )
                    .height(if self.vp_overlap_threshold.is_some() { 40 } else { 20 })
                    .align_y(Alignment::Center)
                    .padding(padding::top(4))
                }
                #[cfg(not(target_os = "macos"))]
                {
                    if let Some((symbol, ratio)) = &self.vp_overlap_threshold {
                        row![
                            text(format!("VP重叠阈值: {} ({:.1}%)", symbol, ratio * 100.0))
                                .size(12),
                        ]
                        .spacing(8)
                    } else {
                        row![]
                    }
                }
            };

            let base = column![
                header_title,
                match sidebar_pos {
                    sidebar::Position::Left => row![sidebar_view, dashboard_view,],
                    sidebar::Position::Right => row![dashboard_view, sidebar_view],
                }
                .spacing(4)
                .padding(8),
            ];

            if let Some(menu) = self.sidebar.active_menu() {
                self.view_with_modal(base.into(), dashboard, menu)
            } else {
                base.into()
            }
        } else if let Some((window_id, window_state)) = &self.historical_data_status_window {
            // Historical data status window
            if *window_id == id {
                window_state.view().map(Message::HistoricalDataStatus)
            } else {
                // Unknown window, return empty
                container(text("Unknown window")).into()
            }
        } else {
            container(
                dashboard
                    .view_window(id, &self.main_window, tickers_table, self.timezone)
                    .map(move |msg| Message::Dashboard(None, msg)),
            )
            .padding(padding::top(style::TITLE_PADDING_TOP))
            .into()
        };

        toast::Manager::new(
            content,
            &self.notifications,
            match sidebar_pos {
                sidebar::Position::Left => Alignment::Start,
                sidebar::Position::Right => Alignment::End,
            },
            Message::RemoveNotification,
        )
        .into()
    }

    fn theme(&self, _window: window::Id) -> iced_core::Theme {
        self.theme.clone().into()
    }

    fn title(&self, _window: window::Id) -> String {
        format!("Flowsurface [{}]", self.layout_manager.active_layout().name)
    }

    fn scale_factor(&self, _window: window::Id) -> f32 {
        self.scale_factor.into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let window_events = window::events().map(Message::WindowEvent);
        let sidebar = self.sidebar.subscription().map(Message::Sidebar);

        let exchange_streams = self
            .active_dashboard()
            .market_subscriptions()
            .map(Message::MarketWsEvent);

        let tick = iced::time::every(std::time::Duration::from_millis(100)).map(Message::Tick);

        let hotkeys = keyboard::on_key_press(|key, _| match key.as_ref() {
            keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::GoBack),
            _ => None,
        });

        // EventBus events are polled in the Tick handler, no separate subscription needed

        let mut subscriptions = vec![
            exchange_streams,
            sidebar,
            window_events,
            tick,
            hotkeys,
        ];
        
        // Add subscription for historical data status window if open
        if let Some((_, window)) = &self.historical_data_status_window {
            subscriptions.push(window.subscription().map(Message::HistoricalDataStatus));
        }

        Subscription::batch(subscriptions)
    }

    fn active_dashboard(&self) -> &Dashboard {
        self.layout_manager
            .active_dashboard()
            .expect("No active dashboard")
    }

    fn active_dashboard_mut(&mut self) -> &mut Dashboard {
        self.layout_manager
            .active_dashboard_mut()
            .expect("No active dashboard")
    }

    fn load_layout(&mut self, layout: layout::Layout, main_window: window::Id) -> Task<Message> {
        self.layout_manager
            .set_active_layout(layout.clone())
            .expect("Failed to set active layout")
            .load_layout(main_window)
            .map(move |msg| Message::Dashboard(Some(layout.id), msg))
    }

    fn view_with_modal<'a>(
        &'a self,
        base: Element<'a, Message>,
        dashboard: &'a Dashboard,
        menu: sidebar::Menu,
    ) -> Element<'a, Message> {
        let sidebar_pos = self.sidebar.position();

        match menu {
            sidebar::Menu::Settings => {
                let settings_modal = {
                    let theme_picklist = {
                        let mut themes: Vec<iced::Theme> = iced_core::Theme::ALL.to_vec();

                        let default_theme = iced_core::Theme::Custom(default_theme().into());
                        themes.push(default_theme);

                        if let Some(custom_theme) = &self.theme_editor.custom_theme {
                            themes.push(custom_theme.clone());
                        }

                        pick_list(themes, Some(self.theme.0.clone()), |theme| {
                            Message::ThemeSelected(data::Theme(theme))
                        })
                    };

                    let toggle_theme_editor = button(text("Theme editor")).on_press(
                        Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(Some(
                            sidebar::Menu::ThemeEditor,
                        ))),
                    );

                    let timezone_picklist = pick_list(
                        [data::UserTimezone::Utc, data::UserTimezone::Local],
                        Some(self.timezone),
                        Message::SetTimezone,
                    );

                    let size_in_quote_currency_checkbox = {
                        let is_active = match self.preferred_currency {
                            exchange::PreferredCurrency::Quote => true,
                            exchange::PreferredCurrency::Base => false,
                        };

                        let checkbox = iced::widget::checkbox("Size in quote currency", is_active)
                            .on_toggle(|checked| {
                                Message::ToggleDialogModal(Some((
                                    "Preferred currency change will take effect after restart"
                                        .to_string(),
                                    Box::new(Message::ToggleShowQuoteCurrency(checked)),
                                )))
                            });

                        tooltip(
                            checkbox,
                            Some(
                                "Display sizes/volumes in quote currency (USD)\n( ! )Has no effect on inverse perps or open interest",
                            ),
                            TooltipPosition::Top,
                        )
                    };

                    let sidebar_pos = pick_list(
                        [sidebar::Position::Left, sidebar::Position::Right],
                        Some(sidebar_pos),
                        |pos| {
                            Message::Sidebar(dashboard::sidebar::Message::SetSidebarPosition(pos))
                        },
                    );

                    let scale_factor = {
                        let current_value: f32 = self.scale_factor.into();

                        let decrease_btn = if current_value > data::config::MIN_SCALE {
                            button(text("-"))
                                .on_press(Message::ScaleFactorChanged((current_value - 0.1).into()))
                        } else {
                            button(text("-"))
                        };

                        let increase_btn = if current_value < data::config::MAX_SCALE {
                            button(text("+"))
                                .on_press(Message::ScaleFactorChanged((current_value + 0.1).into()))
                        } else {
                            button(text("+"))
                        };

                        container(
                            row![
                                decrease_btn,
                                text(format!("{:.0}%", current_value * 100.0)).size(14),
                                increase_btn,
                            ]
                            .align_y(Alignment::Center)
                            .spacing(8)
                            .padding(4),
                        )
                        .style(style::modal_container)
                    };

                    let trade_fetch_checkbox = {
                        let is_active = exchange::fetcher::is_trade_fetch_enabled();

                        let checkbox = iced::widget::checkbox("Fetch trades (Binance)", is_active)
                            .on_toggle(|checked| {
                                if checked {
                                    Message::ToggleDialogModal(Some((
                                        "This might be unreliable and take some time to complete"
                                            .to_string(),
                                        Box::new(Message::ToggleTradeFetch(true)),
                                    )))
                                } else {
                                    Message::ToggleTradeFetch(false)
                                }
                            });

                        tooltip(
                            checkbox,
                            Some("Try to fetch trades for footprint charts"),
                            TooltipPosition::Top,
                        )
                    };

                    let open_data_folder = {
                        let button =
                            button(text("Open data folder")).on_press(Message::DataFolderRequested);

                        tooltip(
                            button,
                            Some("Open the folder where the data & config is stored"),
                            TooltipPosition::Top,
                        )
                    };

                    let open_historical_data_status = {
                        let button = button(text("历史数据状态"))
                            .on_press(Message::OpenHistoricalDataStatusWindow);

                        tooltip(
                            button,
                            Some("查看和管理历史数据的下载状态"),
                            TooltipPosition::Top,
                        )
                    };

                    let column_content = split_column![
                        column![open_data_folder, open_historical_data_status,].spacing(8),
                        column![text("Sidebar position").size(14), sidebar_pos,].spacing(12),
                        column![text("Time zone").size(14), timezone_picklist,].spacing(12),
                        column![text("Market data").size(14), size_in_quote_currency_checkbox,].spacing(12),
                        column![text("Theme").size(14), theme_picklist,].spacing(12),
                        column![text("Interface scale").size(14), scale_factor,].spacing(12),
                        column![
                            text("Experimental").size(14),
                            column![trade_fetch_checkbox, toggle_theme_editor,].spacing(8),
                        ]
                        .spacing(12),
                        ; spacing = 16, align_x = Alignment::Start
                    ];

                    let content = scrollable::Scrollable::with_direction(
                        column_content,
                        scrollable::Direction::Vertical(
                            scrollable::Scrollbar::new().width(8).scroller_width(6),
                        ),
                    );

                    container(content)
                        .align_x(Alignment::Start)
                        .max_width(240)
                        .padding(24)
                        .style(style::dashboard_modal)
                };

                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                let base_content = dashboard_modal(
                    base,
                    settings_modal,
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                );

                if let Some((dialog, on_confirm)) = &self.confirm_dialog {
                    let dialog_content = confirm_dialog_container(
                        dialog,
                        *on_confirm.to_owned(),
                        Message::ToggleDialogModal(None),
                    );

                    main_dialog_modal(
                        base_content,
                        dialog_content,
                        Message::ToggleDialogModal(None),
                    )
                } else {
                    base_content
                }
            }
            sidebar::Menu::Layout => {
                let main_window = self.main_window.id;

                let manage_pane = if let Some((window_id, pane_id)) = dashboard.focus {
                    let selected_pane_str =
                        if let Some(state) = dashboard.get_pane(main_window, window_id, pane_id) {
                            let link_group_name: String =
                                state.link_group.as_ref().map_or_else(String::new, |g| {
                                    " - Group ".to_string() + &g.to_string()
                                });

                            state.content.to_string() + &link_group_name
                        } else {
                            "".to_string()
                        };

                    let is_main_window = window_id == main_window;

                    let reset_pane_button = {
                        let btn = button(text("Reset").align_x(Alignment::Center))
                            .width(iced::Length::Fill);
                        if is_main_window {
                            btn.on_press(Message::Dashboard(
                                None,
                                dashboard::Message::Pane(
                                    main_window,
                                    dashboard::pane::Message::ReplacePane(pane_id),
                                ),
                            ))
                        } else {
                            btn
                        }
                    };
                    let split_pane_button = {
                        let btn = button(text("Split").align_x(Alignment::Center))
                            .width(iced::Length::Fill);
                        if is_main_window {
                            btn.on_press(Message::Dashboard(
                                None,
                                dashboard::Message::Pane(
                                    main_window,
                                    dashboard::pane::Message::SplitPane(
                                        pane_grid::Axis::Horizontal,
                                        pane_id,
                                    ),
                                ),
                            ))
                        } else {
                            btn
                        }
                    };

                    column![
                        text(selected_pane_str),
                        row![
                            tooltip(
                                reset_pane_button,
                                if is_main_window {
                                    Some("Reset selected pane")
                                } else {
                                    None
                                },
                                TooltipPosition::Top,
                            ),
                            tooltip(
                                split_pane_button,
                                if is_main_window {
                                    Some("Split selected pane horizontally")
                                } else {
                                    None
                                },
                                TooltipPosition::Top,
                            ),
                        ]
                        .spacing(8)
                    ]
                    .spacing(8)
                } else {
                    column![text("No pane selected"),].spacing(8)
                };

                let manage_layout_modal = {
                    let col = column![
                        manage_pane,
                        rule::horizontal(1.0).style(style::split_ruler),
                        self.layout_manager.view().map(Message::Layouts)
                    ];

                    container(col.align_x(Alignment::Center).spacing(20))
                        .width(260)
                        .padding(24)
                        .style(style::dashboard_modal)
                };

                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).top(40)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).top(40)),
                };

                dashboard_modal(
                    base,
                    manage_layout_modal,
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::Start,
                    align_x,
                )
            }
            sidebar::Menu::Audio => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).top(76)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).top(76)),
                };

                let depth_streams_list = dashboard.streams.depth_streams(None);

                dashboard_modal(
                    base,
                    self.audio_stream
                        .view(depth_streams_list)
                        .map(Message::AudioStream),
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::Start,
                    align_x,
                )
            }
            sidebar::Menu::ThemeEditor => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                dashboard_modal(
                    base,
                    self.theme_editor
                        .view(&self.theme.0)
                        .map(Message::ThemeEditor),
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                )
            }
        }
    }
}
