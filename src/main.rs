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
    historical_data_service::HistoricalDataService,
    historical_ingester::HistoricalIngesterService,
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
        let data_dir = data::data_path(Some("market_data"));
        std::fs::create_dir_all(&data_dir).ok();

        let mut realtime_data_service = RealtimeDataService::new(data_dir.clone());
        realtime_data_service.set_kline_cache(kline_cache.clone());
        let unified_data_service = Arc::new(UnifiedDataService::new(
            Arc::new(realtime_data_service),
            Arc::new(HistoricalDataService::new(Arc::new(HistoricalIngesterService::new(None)))),
        ));
        // --- End of Arbiter Service Initialization ---

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
            sidebar,
            confirm_dialog: None,
            timezone: saved_state.timezone,
            scale_factor: saved_state.scale_factor,
            preferred_currency: saved_state.preferred_currency,
            theme: saved_state.theme,
            notifications: vec![],
            ingest_tx,
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
                    
                    return Task::future(async move {
                        // Use UnifiedDataService to fetch ticks (automatically handles real-time and historical)
                        let ticks = match unified_service.fetch_ticks(symbol_clone.clone(), range).await {
                            Ok(ticks) => ticks,
                            Err(e) => {
                                let compute_err: data::compute::vp::ComputeError = e.into();
                                return Message::VpComputed(symbol_clone.clone(), Err(compute_err));
                            },
                        };
                        
                        // Calculate compute parameters
                        let num_ticks = ticks.prices.len() as u32;
                        if num_ticks == 0 {
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
                        };
                        
                        // Run compute on GPU
                        let result = service.compute_vp(&ticks, &params, histogram_buckets).await;
                        Message::VpComputed(symbol_clone, result)
                    });
                } else {
                    log::warn!("ComputeVp requested but VpComputeService is not ready.");
                }
            }
            Message::VpComputed(symbol, result) => {
                match result {
                    Ok(profile) => {
                        // Find the chart for this symbol and update its VP data
                        let dashboard = self.active_dashboard_mut();
                        
                        if let Some(pane) = dashboard.find_pane_by_symbol(&symbol) {
                            if let screen::dashboard::pane::Content::Kline { chart: Some(chart), .. } = &mut pane.content {
                                chart.set_volume_profile(profile);
                            } else {
                                log::warn!("Pane content is not Kline or chart is None");
                            }
                        } else {
                            log::warn!("No chart found for symbol {} to store VP data", symbol);
                        }
                    }
                    Err(e) => {
                        log::error!("Volume Profile computation failed for {}: {}", symbol, e);
                        self.notifications.push(Toast::error(format!(
                            "VP Compute Failed for {}: {}",
                            symbol, e
                        )));
                    }
                }
            }
            Message::Tick(now) => {
                let main_window_id = self.main_window.id;

                return self
                    .active_dashboard_mut()
                    .tick(now, main_window_id)
                    .map(move |msg| Message::Dashboard(None, msg));
            }
            Message::WindowEvent(event) => match event {
                window::Event::CloseRequested(window) => {
                    let main_window = self.main_window.id;
                    let dashboard = self.active_dashboard_mut();

                    if window != main_window {
                        dashboard.popout.remove(&window);
                        return window::close(window);
                    }

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
                                Err(err) => {
                                    log::warn!("{err}",);
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
                    iced::widget::center(
                        text("FLOWSURFACE")
                            .font(iced::Font {
                                weight: iced::font::Weight::Bold,
                                ..Default::default()
                            })
                            .size(16)
                            .style(style::title_text),
                    )
                    .height(20)
                    .align_y(Alignment::Center)
                    .padding(padding::top(4))
                }
                #[cfg(not(target_os = "macos"))]
                {
                    column![]
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

        Subscription::batch(vec![
            exchange_streams,
            sidebar,
            window_events,
            tick,
            hotkeys,
        ])
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

                    let column_content = split_column![
                        column![open_data_folder,].spacing(8),
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
