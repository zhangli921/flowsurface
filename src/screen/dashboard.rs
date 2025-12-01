pub mod pane;
pub mod panel;
pub mod sidebar;
pub mod tickers_table;

// 新架构模块（暂未启用）
#[allow(dead_code)]
pub mod unified_data_manager;
#[allow(dead_code)]
pub mod chart_registry;
#[allow(dead_code)]
pub mod chart_traits;

pub use sidebar::Sidebar;

use super::DashboardError;
use crate::{
    chart,
    screen::dashboard::tickers_table::TickersTable,
    style,
    widget::toast::Toast,
    window::{self, Window},
};
use data::{
    UserTimezone,
    layout::{WindowSpec, pane::ContentKind},
};
use exchange::{
    Kline, PushFrequency, StreamPairKind, TickMultiplier, TickerInfo, Timeframe, Trade,
    adapter::{
        self, AdapterError, Exchange, PersistStreamKind, ResolvedStream, StreamConfig, StreamKind,
        StreamTicksize, UniqueStreams, binance, bybit, hyperliquid, okex,
    },
    depth::Depth,
    fetcher::{FetchRange, FetchedData},
};

use iced::{
    Element, Length, Subscription, Task, Vector,
    task::{Straw, sipper},
    widget::{
        PaneGrid, center, container,
        pane_grid::{self, Configuration},
    },
};
use iced_futures::futures::TryFutureExt;
use std::{collections::HashMap, path::PathBuf, time::Instant, vec};

#[derive(Debug, Clone)]
pub enum Message {
    Pane(window::Id, pane::Message),
    ChangePaneStatus(uuid::Uuid, pane::Status),
    SavePopoutSpecs(HashMap<window::Id, WindowSpec>),
    ErrorOccurred(Option<uuid::Uuid>, DashboardError),
    Notification(Toast),
    DistributeFetchedData {
        layout_id: uuid::Uuid,
        pane_id: uuid::Uuid,
        stream: StreamKind,
        data: FetchedData,
    },
    ResolveStreams(uuid::Uuid, Vec<PersistStreamKind>),
}

pub struct Dashboard {
    pub panes: pane_grid::State<pane::State>,
    pub focus: Option<(window::Id, pane_grid::Pane)>,
    pub popout: HashMap<window::Id, (pane_grid::State<pane::State>, WindowSpec)>,
    pub streams: UniqueStreams,
    layout_id: uuid::Uuid,
    
    // 新架构组件（暂未启用）
    #[allow(dead_code)]
    unified_data_manager: Option<std::sync::Arc<unified_data_manager::UnifiedDataManager>>,
    #[allow(dead_code)]
    chart_registry: Option<chart_registry::ChartRegistry>,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            panes: pane_grid::State::with_configuration(Self::default_pane_config()),
            focus: None,
            streams: UniqueStreams::default(),
            popout: HashMap::new(),
            layout_id: uuid::Uuid::new_v4(),
            // 新架构组件默认不启用
            unified_data_manager: None,
            chart_registry: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Notification(Toast),
    DistributeFetchedData {
        layout_id: uuid::Uuid,
        pane_id: uuid::Uuid,
        data: FetchedData,
        stream: StreamKind,
    },
    ResolveStreams {
        pane_id: uuid::Uuid,
        streams: Vec<PersistStreamKind>,
    },
}

impl Dashboard {
    /// 初始化新架构组件（可选，通过环境变量控制）
    /// 
    /// 设置环境变量 `FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true` 来启用
    pub fn init_unified_data_manager(&mut self) {
        // 检查环境变量
        let enabled = std::env::var("FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        
        if enabled && self.unified_data_manager.is_none() {
            let data_manager = std::sync::Arc::new(
                unified_data_manager::UnifiedDataManager::new(1000) // 最大缓存 1000 项
            );
            let chart_registry = chart_registry::ChartRegistry::new(data_manager.clone());
            
            self.unified_data_manager = Some(data_manager);
            self.chart_registry = Some(chart_registry);
            
            log::info!("UnifiedDataManager initialized");
        }
    }
    
    /// 检查新架构是否已启用
    pub fn is_unified_data_manager_enabled(&self) -> bool {
        self.unified_data_manager.is_some()
    }
    
    /// 注册图表到 ChartRegistry（如果新架构已启用）
    /// 
    /// 返回注册的订阅者 ID（如果成功）
    /// 
    /// 注意：图表需要满足 'static 生命周期约束
    pub fn register_chart<C: chart_registry::ChartDataManager + 'static>(
        &mut self,
        chart: C,
        chart_type: chart_registry::ChartType,
    ) -> Option<uuid::Uuid> {
        if let Some(registry) = &mut self.chart_registry {
            let id = registry.register_chart(chart, chart_type);
            Some(id)
        } else {
            None
        }
    }
    
    /// 注销图表（如果新架构已启用）
    pub fn unregister_chart(&mut self, subscriber_id: uuid::Uuid) {
        if let Some(registry) = &mut self.chart_registry {
            registry.unregister_chart(subscriber_id);
        }
    }
    
    /// 分发数据到 UnifiedDataManager（如果启用）
    /// 
    /// 这个方法会在数据到达时调用，将数据缓存并通知所有订阅者
    fn distribute_to_unified_manager(&self, data: &FetchedData, stream_type: &StreamKind) {
        if let Some(data_manager) = &self.unified_data_manager {
            // 从 stream_type 中提取 ticker_info
            let ticker_info = match stream_type {
                StreamKind::Kline { ticker_info, .. } => *ticker_info,
                StreamKind::DepthAndTrades { ticker_info, .. } => *ticker_info,
                _ => {
                    // 其他类型的 stream 暂不支持
                    return;
                }
            };
            
            // 构建 DataKey
            // 注意：我们需要从 data 中提取 range 和 basis
            // 由于 FetchedData 不直接包含这些信息，我们需要从 stream_type 推断
            let (range, basis) = match (data, stream_type) {
                (FetchedData::Trades { batch, until_time }, _) => {
                    let from = batch.first().map(|t| t.time).unwrap_or(0);
                    let to = *until_time;
                    (
                        FetchRange::Trades(from, to),
                        data::chart::Basis::Time(exchange::Timeframe::M1), // 默认值，实际应该从 pane 获取
                    )
                }
                (FetchedData::Klines { data: klines, .. }, StreamKind::Kline { timeframe, .. }) => {
                    let from = klines.first().map(|k| k.time).unwrap_or(0);
                    // Kline 的 time 是开始时间，结束时间需要根据 timeframe 计算
                    let interval_ms = timeframe.to_milliseconds();
                    let to = klines.last().map(|k| k.time + interval_ms).unwrap_or(0);
                    (
                        FetchRange::Kline(from, to),
                        data::chart::Basis::Time(*timeframe),
                    )
                }
                (FetchedData::OI { data: oi, .. }, StreamKind::Kline { timeframe, .. }) => {
                    let from = oi.first().map(|o| o.time).unwrap_or(0);
                    let to = oi.last().map(|o| o.time).unwrap_or(0);
                    (
                        FetchRange::OpenInterest(from, to),
                        data::chart::Basis::Time(*timeframe),
                    )
                }
                _ => {
                    // 不支持的数据类型
                    return;
                }
            };
            
            let key = unified_data_manager::DataKey::new(ticker_info, range, basis);
            
            // 通知 UnifiedDataManager 数据已到达
            let subscribers = data_manager.on_data_fetched(key, data.clone());
            
            if !subscribers.is_empty() {
                log::debug!(
                    "Distributed data to {} subscribers via UnifiedDataManager",
                    subscribers.len()
                );
                // 注意：这里我们只是记录日志，实际的图表更新会在原有的逻辑中处理
                // 未来可以在这里添加额外的通知机制
            }
        }
    }
    
    fn default_pane_config() -> Configuration<pane::State> {
        Configuration::Split {
            axis: pane_grid::Axis::Vertical,
            ratio: 0.8,
            a: Box::new(Configuration::Split {
                axis: pane_grid::Axis::Horizontal,
                ratio: 0.4,
                a: Box::new(Configuration::Split {
                    axis: pane_grid::Axis::Vertical,
                    ratio: 0.5,
                    a: Box::new(Configuration::Pane(pane::State::default())),
                    b: Box::new(Configuration::Pane(pane::State::default())),
                }),
                b: Box::new(Configuration::Split {
                    axis: pane_grid::Axis::Vertical,
                    ratio: 0.5,
                    a: Box::new(Configuration::Pane(pane::State::default())),
                    b: Box::new(Configuration::Pane(pane::State::default())),
                }),
            }),
            b: Box::new(Configuration::Pane(pane::State::default())),
        }
    }

    pub fn from_config(
        panes: Configuration<pane::State>,
        popout_windows: Vec<(Configuration<pane::State>, WindowSpec)>,
        layout_id: uuid::Uuid,
    ) -> Self {
        let panes = pane_grid::State::with_configuration(panes);

        let mut popout = HashMap::new();

        for (pane, specs) in popout_windows {
            popout.insert(
                window::Id::unique(),
                (pane_grid::State::with_configuration(pane), specs),
            );
        }

        let mut dashboard = Self {
            panes,
            focus: None,
            streams: UniqueStreams::default(),
            popout,
            layout_id,
            // 新架构组件默认不启用
            unified_data_manager: None,
            chart_registry: None,
        };
        
        // 尝试初始化新架构（如果环境变量启用）
        dashboard.init_unified_data_manager();
        
        dashboard
    }

    pub fn load_layout(&mut self, main_window: window::Id) -> Task<Message> {
        let mut open_popouts_tasks: Vec<Task<Message>> = vec![];
        let mut new_popout = Vec::new();
        let mut keys_to_remove = Vec::new();

        for (old_window_id, (_, specs)) in &self.popout {
            keys_to_remove.push((*old_window_id, *specs));
        }

        // remove keys and open new windows
        for (old_window_id, window_spec) in keys_to_remove {
            let (window, task) = window::open(window::Settings {
                position: window::Position::Specific(window_spec.position()),
                size: window_spec.size(),
                exit_on_close_request: false,
                ..window::settings()
            });

            open_popouts_tasks.push(task.then(|_| Task::none()));

            if let Some((removed_pane, specs)) = self.popout.remove(&old_window_id) {
                new_popout.push((window, (removed_pane, specs)));
            }
        }

        // assign new windows to old panes
        for (window, (pane, specs)) in new_popout {
            self.popout.insert(window, (pane, specs));
        }

        Task::batch(open_popouts_tasks).chain(self.refresh_streams(main_window))
    }

    pub fn update(
        &mut self,
        message: Message,
        main_window: &Window,
        layout_id: &uuid::Uuid,
    ) -> (Task<Message>, Option<Event>) {
        match message {
            Message::SavePopoutSpecs(specs) => {
                for (window_id, new_spec) in specs {
                    if let Some((_, spec)) = self.popout.get_mut(&window_id) {
                        *spec = new_spec;
                    }
                }
            }
            Message::ErrorOccurred(pane_id, err) => match pane_id {
                Some(id) => {
                    if let Some(state) = self.get_mut_pane_state_by_uuid(main_window.id, id) {
                        state.status = pane::Status::Ready;
                        state.notifications.push(Toast::error(err.to_string()));
                        // 如果是 trades 下载错误，重置 fetching_trades 标志
                        if let pane::Content::Kline { chart, .. } = &mut state.content {
                            if let Some(c) = chart {
                                c.reset_request_handler();
                            }
                        }
                    }
                }
                _ => {
                    return (
                        Task::done(Message::Notification(Toast::error(err.to_string()))),
                        None,
                    );
                }
            },
            Message::Pane(window, message) => match message {
                pane::Message::PaneClicked(pane) => {
                    self.focus = Some((window, pane));
                }
                pane::Message::PaneResized(pane_grid::ResizeEvent { split, ratio }) => {
                    self.panes.resize(split, ratio);
                }
                pane::Message::PaneDragged(event) => {
                    if let pane_grid::DragEvent::Dropped { pane, target } = event {
                        self.panes.drop(pane, target);
                    }
                }
                pane::Message::SplitPane(axis, pane) => {
                    let focus_pane = if let Some((new_pane, _)) =
                        self.panes.split(axis, pane, pane::State::new())
                    {
                        Some(new_pane)
                    } else {
                        None
                    };

                    if Some(focus_pane).is_some() {
                        self.focus = Some((window, focus_pane.unwrap()));
                    }
                }
                pane::Message::ClosePane(pane) => {
                    if let Some((_, sibling)) = self.panes.close(pane) {
                        self.focus = Some((window, sibling));
                    }
                }
                pane::Message::MaximizePane(pane) => {
                    self.panes.maximize(pane);
                }
                pane::Message::Restore => {
                    self.panes.restore();
                }
                pane::Message::ReplacePane(pane) => {
                    if let Some(pane) = self.panes.get_mut(pane) {
                        *pane = pane::State::new();
                    }

                    return (self.refresh_streams(main_window.id), None);
                }
                pane::Message::VisualConfigChanged(pane, cfg, to_sync) => {
                    if to_sync {
                        if let Some(state) = self.get_pane(main_window.id, window, pane) {
                            let studies_cfg = state.content.studies();
                            let clusters_cfg = match &state.content {
                                pane::Content::Kline {
                                    kind: data::chart::KlineChartKind::Footprint { clusters, .. },
                                    ..
                                } => Some(*clusters),
                                _ => None,
                            };

                            self.iter_all_panes_mut(main_window.id)
                                .for_each(|(_, _, state)| {
                                    let should_apply = match state.settings.visual_config {
                                        Some(ref current_cfg) => {
                                            std::mem::discriminant(current_cfg)
                                                == std::mem::discriminant(&cfg)
                                        }
                                        None => matches!(
                                            (&cfg, &state.content),
                                            (
                                                data::layout::pane::VisualConfig::Kline(_),
                                                pane::Content::Kline { .. }
                                            ) | (
                                                data::layout::pane::VisualConfig::Heatmap(_),
                                                pane::Content::Heatmap { .. }
                                            ) | (
                                                data::layout::pane::VisualConfig::TimeAndSales(_),
                                                pane::Content::TimeAndSales(_)
                                            ) | (
                                                data::layout::pane::VisualConfig::Comparison(_),
                                                pane::Content::Comparison(_)
                                            )
                                        ),
                                    };

                                    if should_apply {
                                        state.settings.visual_config = Some(cfg.clone());
                                        state.content.change_visual_config(cfg.clone());

                                        if let Some(studies) = &studies_cfg {
                                            state.content.update_studies(studies.clone());
                                        }

                                        if let Some(cluster_kind) = &clusters_cfg
                                            && let pane::Content::Kline { chart, .. } =
                                                &mut state.content
                                            && let Some(c) = chart
                                        {
                                            c.set_cluster_kind(*cluster_kind);
                                        }
                                    }
                                });
                        }
                    } else if let Some(state) = self.get_mut_pane(main_window.id, window, pane) {
                        state.settings.visual_config = Some(cfg.clone());
                        state.content.change_visual_config(cfg);
                    }
                }
                pane::Message::SwitchLinkGroup(pane, group) => {
                    if group.is_none() {
                        if let Some(state) = self.get_mut_pane(main_window.id, window, pane) {
                            state.link_group = None;
                        }
                        return (Task::none(), None);
                    }

                    let maybe_ticker_info = self
                        .iter_all_panes(main_window.id)
                        .filter(|(w, p, _)| !(*w == window && *p == pane))
                        .find_map(|(_, _, other_state)| {
                            if other_state.link_group == group {
                                other_state.stream_pair()
                            } else {
                                None
                            }
                        });

                    if let Some(state) = self.get_mut_pane(main_window.id, window, pane) {
                        state.link_group = group;
                        state.modal = None;

                        if let Some(ticker_info) = maybe_ticker_info
                            && state.stream_pair() != Some(ticker_info)
                        {
                            let pane_id = state.unique_id();
                            let content_kind = state.content.kind();

                            let streams =
                                state.set_content_and_streams(vec![ticker_info], content_kind);
                            self.streams.extend(streams.iter());

                            for stream in &streams {
                                if let StreamKind::Kline { .. } = stream {
                                    return (
                                        kline_fetch_task(*layout_id, pane_id, *stream, None, None),
                                        None,
                                    );
                                }
                            }
                        }
                    }
                }
                pane::Message::Popout => {
                    return (self.popout_pane(main_window), None);
                }
                pane::Message::Merge => {
                    return (self.merge_pane(main_window), None);
                }
                pane::Message::PaneEvent(pane, local) => {
                    if let Some(state) = self.get_mut_pane(main_window.id, window, pane) {
                        let Some(effect) = state.update(local) else {
                            return (Task::none(), None);
                        };

                        let task = match effect {
                            pane::Effect::RefreshStreams => self.refresh_streams(main_window.id),
                            pane::Effect::RequestFetch(reqs) => request_fetch_many(
                                state,
                                *layout_id,
                                reqs.into_iter().map(|r| (r.req_id, r.fetch, r.stream)),
                            )
                            .chain(self.refresh_streams(main_window.id)),
                            pane::Effect::SwitchTickersInGroup(ticker_info) => {
                                self.switch_tickers_in_group(main_window.id, ticker_info)
                            }
                            pane::Effect::FocusWidget(id) => {
                                return (iced::widget::operation::focus(id), None);
                            }
                        };
                        return (task, None);
                    }
                }
            },
            Message::ChangePaneStatus(pane_id, status) => {
                if let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window.id, pane_id) {
                    pane_state.status = status;
                }
            }
            Message::DistributeFetchedData {
                layout_id,
                pane_id,
                data,
                stream,
            } => {
                return (
                    Task::none(),
                    Some(Event::DistributeFetchedData {
                        layout_id,
                        pane_id,
                        data,
                        stream,
                    }),
                );
            }
            Message::ResolveStreams(pane_id, streams) => {
                return (
                    Task::none(),
                    Some(Event::ResolveStreams { pane_id, streams }),
                );
            }
            Message::Notification(toast) => {
                return (Task::none(), Some(Event::Notification(toast)));
            }
        }

        (Task::none(), None)
    }

    fn new_pane(
        &mut self,
        axis: pane_grid::Axis,
        main_window: &Window,
        pane_state: Option<pane::State>,
    ) -> Task<Message> {
        if self
            .focus
            .filter(|(window, _)| *window == main_window.id)
            .is_some()
        {
            // If there is any focused pane on main window, split it
            return self.split_pane(axis, main_window);
        } else {
            // If there is no focused pane, split the last pane or create a new empty grid
            let pane = self.panes.iter().last().map(|(pane, _)| pane).copied();

            if let Some(pane) = pane {
                let result = self.panes.split(axis, pane, pane_state.unwrap_or_default());

                if let Some((pane, _)) = result {
                    return self.focus_pane(main_window.id, pane);
                }
            } else {
                let (state, pane) = pane_grid::State::new(pane_state.unwrap_or_default());
                self.panes = state;

                return self.focus_pane(main_window.id, pane);
            }
        }

        Task::none()
    }

    fn focus_pane(&mut self, window: window::Id, pane: pane_grid::Pane) -> Task<Message> {
        if self.focus != Some((window, pane)) {
            self.focus = Some((window, pane));
        }

        Task::none()
    }

    fn split_pane(&mut self, axis: pane_grid::Axis, main_window: &Window) -> Task<Message> {
        if let Some((window, pane)) = self.focus
            && window == main_window.id
        {
            let result = self.panes.split(axis, pane, pane::State::new());

            if let Some((pane, _)) = result {
                return self.focus_pane(main_window.id, pane);
            }
        }

        Task::none()
    }

    fn popout_pane(&mut self, main_window: &Window) -> Task<Message> {
        if let Some((_, id)) = self.focus.take()
            && let Some((pane, _)) = self.panes.close(id)
        {
            let (window, task) = window::open(window::Settings {
                position: main_window
                    .position
                    .map(|point| window::Position::Specific(point + Vector::new(20.0, 20.0)))
                    .unwrap_or_default(),
                exit_on_close_request: false,
                min_size: Some(iced::Size::new(400.0, 300.0)),
                ..window::settings()
            });

            let (state, id) = pane_grid::State::new(pane);
            self.popout.insert(window, (state, WindowSpec::default()));

            return task.then(move |window| {
                Task::done(Message::Pane(window, pane::Message::PaneClicked(id)))
            });
        }

        Task::none()
    }

    fn merge_pane(&mut self, main_window: &Window) -> Task<Message> {
        if let Some((window, pane)) = self.focus.take()
            && let Some(pane_state) = self
                .popout
                .remove(&window)
                .and_then(|(mut panes, _)| panes.panes.remove(&pane))
        {
            let task = self.new_pane(pane_grid::Axis::Horizontal, main_window, Some(pane_state));

            return Task::batch(vec![window::close(window), task]);
        }

        Task::none()
    }

    pub fn get_pane(
        &self,
        main_window: window::Id,
        window: window::Id,
        pane: pane_grid::Pane,
    ) -> Option<&pane::State> {
        if main_window == window {
            self.panes.get(pane)
        } else {
            self.popout
                .get(&window)
                .and_then(|(panes, _)| panes.get(pane))
        }
    }

    fn get_mut_pane(
        &mut self,
        main_window: window::Id,
        window: window::Id,
        pane: pane_grid::Pane,
    ) -> Option<&mut pane::State> {
        if main_window == window {
            self.panes.get_mut(pane)
        } else {
            self.popout
                .get_mut(&window)
                .and_then(|(panes, _)| panes.get_mut(pane))
        }
    }

    fn get_mut_pane_state_by_uuid(
        &mut self,
        main_window: window::Id,
        uuid: uuid::Uuid,
    ) -> Option<&mut pane::State> {
        self.iter_all_panes_mut(main_window)
            .find(|(_, _, state)| state.unique_id() == uuid)
            .map(|(_, _, state)| state)
    }

    fn iter_all_panes(
        &self,
        main_window: window::Id,
    ) -> impl Iterator<Item = (window::Id, pane_grid::Pane, &pane::State)> {
        self.panes
            .iter()
            .map(move |(pane, state)| (main_window, *pane, state))
            .chain(self.popout.iter().flat_map(|(window_id, (panes, _))| {
                panes.iter().map(|(pane, state)| (*window_id, *pane, state))
            }))
    }

    fn iter_all_panes_mut(
        &mut self,
        main_window: window::Id,
    ) -> impl Iterator<Item = (window::Id, pane_grid::Pane, &mut pane::State)> {
        self.panes
            .iter_mut()
            .map(move |(pane, state)| (main_window, *pane, state))
            .chain(self.popout.iter_mut().flat_map(|(window_id, (panes, _))| {
                panes
                    .iter_mut()
                    .map(|(pane, state)| (*window_id, *pane, state))
            }))
    }

    pub fn view<'a>(
        &'a self,
        main_window: &'a Window,
        tickers_table: &'a TickersTable,
        timezone: UserTimezone,
    ) -> Element<'a, Message> {
        let pane_grid: Element<_> = PaneGrid::new(&self.panes, |id, pane, maximized| {
            let is_focused = self.focus == Some((main_window.id, id));
            pane.view(
                id,
                self.panes.len(),
                is_focused,
                maximized,
                main_window.id,
                main_window,
                timezone,
                tickers_table,
            )
        })
        .min_size(240)
        .on_click(pane::Message::PaneClicked)
        .on_drag(pane::Message::PaneDragged)
        .on_resize(8, pane::Message::PaneResized)
        .spacing(6)
        .style(style::pane_grid)
        .into();

        pane_grid.map(move |message| Message::Pane(main_window.id, message))
    }

    pub fn view_window<'a>(
        &'a self,
        window: window::Id,
        main_window: &'a Window,
        tickers_table: &'a TickersTable,
        timezone: UserTimezone,
    ) -> Element<'a, Message> {
        if let Some((state, _)) = self.popout.get(&window) {
            let content = container(
                PaneGrid::new(state, |id, pane, _maximized| {
                    let is_focused = self.focus == Some((window, id));
                    pane.view(
                        id,
                        state.len(),
                        is_focused,
                        false,
                        window,
                        main_window,
                        timezone,
                        tickers_table,
                    )
                })
                .on_click(pane::Message::PaneClicked),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(8);

            Element::new(content).map(move |message| Message::Pane(window, message))
        } else {
            Element::new(center("No pane found for window"))
                .map(move |message| Message::Pane(window, message))
        }
    }

    pub fn go_back(&mut self, main_window: window::Id) -> bool {
        let Some((window, pane)) = self.focus else {
            return false;
        };

        let Some(state) = self.get_mut_pane(main_window, window, pane) else {
            return false;
        };

        if state.modal.is_some() {
            state.modal = None;
            return true;
        }
        false
    }

    fn handle_error(
        &mut self,
        pane_id: Option<uuid::Uuid>,
        err: &DashboardError,
        main_window: window::Id,
    ) -> Task<Message> {
        match pane_id {
            Some(id) => {
                if let Some(state) = self.get_mut_pane_state_by_uuid(main_window, id) {
                    state.status = pane::Status::Ready;
                    state.notifications.push(Toast::error(err.to_string()));
                }
                Task::none()
            }
            _ => Task::done(Message::Notification(Toast::error(err.to_string()))),
        }
    }

    fn init_pane(
        &mut self,
        main_window: window::Id,
        window: window::Id,
        selected_pane: pane_grid::Pane,
        ticker_info: TickerInfo,
        content_kind: ContentKind,
    ) -> Task<Message> {
        if let Some(state) = self.get_mut_pane(main_window, window, selected_pane) {
            let pane_id = state.unique_id();

            let streams = state.set_content_and_streams(vec![ticker_info], content_kind);
            self.streams.extend(streams.iter());

            for stream in &streams {
                if let StreamKind::Kline { .. } = stream {
                    return kline_fetch_task(self.layout_id, pane_id, *stream, None, None);
                }
            }
        }

        Task::none()
    }

    pub fn init_focused_pane(
        &mut self,
        main_window: window::Id,
        ticker_info: TickerInfo,
        content_kind: ContentKind,
    ) -> Task<Message> {
        if self.focus.is_none()
            && self.panes.len() == 1
            && let Some((pane_id, _)) = self.panes.iter().next()
        {
            self.focus = Some((main_window, *pane_id));
        }

        if let Some((window, selected_pane)) = self.focus
            && let Some(state) = self.get_mut_pane(main_window, window, selected_pane)
        {
            let previous_ticker = state.stream_pair();
            if previous_ticker.is_some() && previous_ticker != Some(ticker_info) {
                state.link_group = None;
            }

            let streams = state.set_content_and_streams(vec![ticker_info], content_kind);

            let pane_id = state.unique_id();
            self.streams.extend(streams.iter());

            for stream in &streams {
                if let StreamKind::Kline { .. } = stream {
                    return kline_fetch_task(self.layout_id, pane_id, *stream, None, None);
                }
            }
            return Task::none();
        }

        Task::done(Message::Notification(Toast::warn(
            "No focused pane found".to_string(),
        )))
    }

    pub fn switch_tickers_in_group(
        &mut self,
        main_window: window::Id,
        ticker_info: TickerInfo,
    ) -> Task<Message> {
        if self.focus.is_none()
            && self.panes.len() == 1
            && let Some((pane_id, _)) = self.panes.iter().next()
        {
            self.focus = Some((main_window, *pane_id));
        }

        let link_group = self.focus.and_then(|(window, pane)| {
            self.get_pane(main_window, window, pane)
                .and_then(|state| state.link_group)
        });

        if let Some(group) = link_group {
            let pane_infos: Vec<(window::Id, pane_grid::Pane, ContentKind)> = self
                .iter_all_panes_mut(main_window)
                .filter_map(|(window, pane, state)| {
                    if state.link_group == Some(group) {
                        Some((window, pane, state.content.kind()))
                    } else {
                        None
                    }
                })
                .collect();

            let tasks: Vec<Task<Message>> = pane_infos
                .iter()
                .map(|(window, pane, content_kind)| {
                    self.init_pane(main_window, *window, *pane, ticker_info, *content_kind)
                })
                .collect();

            Task::batch(tasks)
        } else if let Some((window, pane)) = self.focus {
            if let Some(state) = self.get_mut_pane(main_window, window, pane) {
                let content_kind = state.content.kind();
                self.init_focused_pane(main_window, ticker_info, content_kind)
            } else {
                Task::done(Message::Notification(Toast::warn(
                    "Couldn't get focused pane's content".to_string(),
                )))
            }
        } else {
            Task::done(Message::Notification(Toast::warn(
                "No link group or focused pane found".to_string(),
            )))
        }
    }

    pub fn toggle_trade_fetch(&mut self, is_enabled: bool, main_window: &Window) {
        exchange::fetcher::toggle_trade_fetch(is_enabled);

        self.iter_all_panes_mut(main_window.id)
            .for_each(|(_, _, state)| {
                if let pane::Content::Kline { chart, kind, .. } = &mut state.content
                    && let Some(c) = chart
                {
                    // 对于 Footprint 类型，总是需要交易数据
                    // 对于 Candles 类型，如果启用了 HVN，也需要交易数据
                    let needs_trades = match kind {
                        data::chart::KlineChartKind::Footprint { .. } => true,
                        data::chart::KlineChartKind::Candles { studies } => {
                            studies.iter().any(|s| matches!(s, data::chart::kline::FootprintStudy::HVN { .. }))
                        }
                    };
                    
                    if needs_trades {
                        c.reset_request_handler();

                        if !is_enabled {
                            state.status = pane::Status::Ready;
                        }
                    }
                }
            });
    }

    pub     fn distribute_fetched_data(
        &mut self,
        main_window: window::Id,
        pane_id: uuid::Uuid,
        data: FetchedData,
        stream_type: StreamKind,
    ) -> Task<Message> {
        // 新架构：如果 UnifiedDataManager 已启用，先分发数据
        if let Some(data_manager) = &self.unified_data_manager {
            self.distribute_to_unified_manager(&data, &stream_type);
        }
        
        match data {
            FetchedData::Trades { batch, until_time } => {
                let last_trade_time = batch.last().map_or(0, |trade| trade.time);
                log::debug!(
                    "distribute_fetched_data: Received trades batch, count={}, last_trade_time={}, until_time={}",
                    batch.len(),
                    last_trade_time,
                    until_time
                );

                if last_trade_time < until_time {
                    log::debug!(
                        "distribute_fetched_data: More batches expected, is_batches_done=false"
                    );
                    if let Err(reason) =
                        self.insert_fetched_trades(main_window, pane_id, &batch, false)
                    {
                        return self.handle_error(Some(pane_id), &reason, main_window);
                    }
                } else {
                    let filtered_batch = batch
                        .iter()
                        .filter(|trade| trade.time <= until_time)
                        .copied()
                        .collect::<Vec<_>>();

                    log::debug!(
                        "distribute_fetched_data: Last batch, is_batches_done=true, filtered_count={}",
                        filtered_batch.len()
                    );
                    if let Err(reason) =
                        self.insert_fetched_trades(main_window, pane_id, &filtered_batch, true)
                    {
                        return self.handle_error(Some(pane_id), &reason, main_window);
                    }
                }
            }
            FetchedData::Klines { data, req_id } => {
                if let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window, pane_id) {
                    pane_state.status = pane::Status::Ready;

                    if let StreamKind::Kline {
                        timeframe,
                        ticker_info,
                    } = stream_type
                    {
                        pane_state.insert_hist_klines(req_id, timeframe, ticker_info, &data);
                    }
                }
            }
            FetchedData::OI { data, req_id } => {
                if let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window, pane_id) {
                    pane_state.status = pane::Status::Ready;

                    if let StreamKind::Kline { .. } = stream_type {
                        pane_state.insert_hist_oi(req_id, &data);
                    }
                }
            }
        }

        Task::none()
    }

    fn insert_fetched_trades(
        &mut self,
        main_window: window::Id,
        pane_id: uuid::Uuid,
        trades: &[Trade],
        is_batches_done: bool,
    ) -> Result<(), DashboardError> {
        let pane_state = self
            .get_mut_pane_state_by_uuid(main_window, pane_id)
            .ok_or_else(|| {
                DashboardError::Unknown(
                    "No matching pane state found for fetched trades".to_string(),
                )
            })?;

        match &mut pane_state.status {
            pane::Status::Loading(exchange::fetcher::InfoKind::FetchingTrades(count)) => {
                *count += trades.len();
            }
            _ => {
                pane_state.status = pane::Status::Loading(
                    exchange::fetcher::InfoKind::FetchingTrades(trades.len()),
                );
            }
        }

        match &mut pane_state.content {
            pane::Content::Kline { chart, .. } => {
                if let Some(c) = chart {
                    c.insert_raw_trades(trades.to_owned(), is_batches_done);

                    if is_batches_done {
                        pane_state.status = pane::Status::Ready;
                    }
                    Ok(())
                } else {
                    Err(DashboardError::Unknown(
                        "fetched trades but no chart found".to_string(),
                    ))
                }
            }
            _ => Err(DashboardError::Unknown(
                "No matching chart found for fetched trades".to_string(),
            )),
        }
    }

    pub fn update_latest_klines(
        &mut self,
        stream: &StreamKind,
        kline: &Kline,
        main_window: window::Id,
    ) -> Task<Message> {
        let mut found_match = false;

        self.iter_all_panes_mut(main_window)
            .for_each(|(_, _, pane_state)| {
                if pane_state.matches_stream(stream) {
                    match &mut pane_state.content {
                        pane::Content::Kline { chart: Some(c), .. } => {
                            c.update_latest_kline(kline);
                        }
                        pane::Content::Comparison(Some(c)) => {
                            c.update_latest_kline(&stream.ticker_info(), kline);
                        }
                        _ => {}
                    }
                    found_match = true;
                }
            });

        if found_match {
            Task::none()
        } else {
            log::debug!("{stream:?} stream had no matching panes - dropping");
            self.refresh_streams(main_window)
        }
    }

    pub fn update_depth_and_trades(
        &mut self,
        stream: &StreamKind,
        depth_update_t: u64,
        depth: &Depth,
        trades_buffer: &[Trade],
        main_window: window::Id,
    ) -> Task<Message> {
        let mut found_match = false;

        self.iter_all_panes_mut(main_window)
            .for_each(|(_, _, pane_state)| {
                if pane_state.matches_stream(stream) {
                    match &mut pane_state.content {
                        pane::Content::Heatmap { chart, .. } => {
                            if let Some(c) = chart {
                                c.insert_datapoint(trades_buffer, depth_update_t, depth);
                            }
                        }
                        pane::Content::Kline { chart, .. } => {
                            if let Some(c) = chart {
                                c.insert_trades_buffer(trades_buffer);
                            }
                        }
                        pane::Content::TimeAndSales(panel) => {
                            if let Some(p) = panel {
                                p.insert_buffer(trades_buffer);
                            }
                        }
                        pane::Content::Ladder(panel) => {
                            if let Some(panel) = panel {
                                panel.insert_buffers(depth_update_t, depth, trades_buffer);
                            }
                        }
                        _ => {
                            log::error!("No chart found for the stream: {stream:?}");
                        }
                    }
                    found_match = true;
                }
            });

        if found_match {
            Task::none()
        } else {
            log::debug!("No matching pane found for the stream: {stream:?}");
            self.refresh_streams(main_window)
        }
    }

    pub fn invalidate_all_panes(&mut self, main_window: window::Id) {
        self.iter_all_panes_mut(main_window)
            .for_each(|(_, _, state)| {
                let _ = state.invalidate(Instant::now());
            });
    }

    pub fn tick(&mut self, now: Instant, main_window: window::Id) -> Task<Message> {
        let mut tasks = vec![];
        let layout_id = self.layout_id;

        self.iter_all_panes_mut(main_window)
            .for_each(|(_window_id, _pane, state)| match state.tick(now) {
                Some(pane::Action::Chart(action)) => match action {
                    chart::Action::ErrorOccurred(err) => {
                        state.status = pane::Status::Ready;
                        state.notifications.push(Toast::error(err.to_string()));
                        // 重置 fetching_trades 标志，以便可以重试
                        if let pane::Content::Kline { chart, .. } = &mut state.content {
                            if let Some(c) = chart {
                                c.reset_request_handler();
                            }
                        }
                    }
                    chart::Action::RequestFetch(reqs) => {
                        tasks.push(request_fetch_many(
                            state,
                            layout_id,
                            reqs.into_iter().map(|r| (r.req_id, r.fetch, r.stream)),
                        ));
                    }
                },
                Some(pane::Action::Panel(_action)) => {}
                Some(pane::Action::ResolveStreams(streams)) => {
                    tasks.push(Task::done(Message::ResolveStreams(
                        state.unique_id(),
                        streams,
                    )));
                }
                Some(pane::Action::ResolveContent) => match state.stream_pair_kind() {
                    Some(StreamPairKind::MultiSource(tickers)) => {
                        state.set_content_and_streams(tickers, state.content.kind());
                    }
                    Some(StreamPairKind::SingleSource(ticker)) => {
                        state.set_content_and_streams(vec![ticker], state.content.kind());
                    }
                    None => {}
                },
                None => {}
            });

        Task::batch(tasks)
    }

    pub fn resolve_streams(
        &mut self,
        main_window: window::Id,
        pane_id: uuid::Uuid,
        streams: Vec<StreamKind>,
    ) -> Task<Message> {
        if let Some(state) = self.get_mut_pane_state_by_uuid(main_window, pane_id) {
            state.streams = ResolvedStream::Ready(streams.clone());
        }
        self.refresh_streams(main_window)
    }

    pub fn market_subscriptions(&self) -> Subscription<exchange::Event> {
        let unique_streams = self
            .streams
            .combined_used()
            .flat_map(|(exchange, specs)| {
                let mut subs = vec![];

                if !specs.depth.is_empty() {
                    let depth_subs = specs
                        .depth
                        .iter()
                        .map(|(ticker, aggr, push_freq)| {
                            let tick_mltp = match aggr {
                                StreamTicksize::Client => None,
                                StreamTicksize::ServerSide(tick_mltp) => Some(*tick_mltp),
                            };
                            depth_subscription(*ticker, tick_mltp, *push_freq)
                        })
                        .collect::<Vec<_>>();

                    if !depth_subs.is_empty() {
                        subs.push(Subscription::batch(depth_subs));
                    }
                }

                let kline_params = specs
                    .kline
                    .iter()
                    .map(|(ticker, timeframe)| (*ticker, *timeframe))
                    .collect::<Vec<_>>();

                if !kline_params.is_empty() {
                    subs.push(kline_subscription(exchange, kline_params));
                }

                subs
            })
            .collect::<Vec<Subscription<exchange::Event>>>();

        Subscription::batch(unique_streams)
    }

    fn refresh_streams(&mut self, main_window: window::Id) -> Task<Message> {
        let all_pane_streams = self
            .iter_all_panes(main_window)
            .flat_map(|(_, _, pane_state)| pane_state.streams.ready_iter().into_iter().flatten());
        self.streams = UniqueStreams::from(all_pane_streams);

        Task::none()
    }
}

fn request_fetch(
    state: &mut pane::State,
    layout_id: uuid::Uuid,
    req_id: uuid::Uuid,
    fetch: FetchRange,
    stream: Option<StreamKind>,
) -> Task<Message> {
    let pane_id = state.unique_id();

    match fetch {
        FetchRange::Kline(from, to) => {
            let kline_stream = {
                if let Some(s) = stream {
                    Some((s, pane_id))
                } else {
                    state.streams.find_ready_map(|stream| {
                        if let StreamKind::Kline { .. } = stream {
                            Some((*stream, pane_id))
                        } else {
                            None
                        }
                    })
                }
            };

            if let Some((stream, pane_uid)) = kline_stream {
                return kline_fetch_task(
                    layout_id,
                    pane_uid,
                    stream,
                    Some(req_id),
                    Some((from, to)),
                );
            }
        }
        FetchRange::OpenInterest(from, to) => {
            let kline_stream = {
                if let Some(s) = stream {
                    Some((s, pane_id))
                } else {
                    state.streams.find_ready_map(|stream| {
                        if let StreamKind::Kline { .. } = stream {
                            Some((*stream, pane_id))
                        } else {
                            None
                        }
                    })
                }
            };

            if let Some((stream, pane_uid)) = kline_stream {
                return oi_fetch_task(layout_id, pane_uid, stream, Some(req_id), Some((from, to)));
            }
        }
        FetchRange::Trades(from_time, to_time) => {
            log::debug!(
                "request_fetch: Starting trades fetch, range: Trades({}, {})",
                from_time,
                to_time
            );
            let trade_info = state.streams.find_ready_map(|stream| {
                if let StreamKind::DepthAndTrades { ticker_info, .. } = stream {
                    Some((*ticker_info, pane_id, *stream))
                } else {
                    None
                }
            });

            if let Some((ticker_info, pane_id, stream)) = trade_info {
                let is_binance = matches!(
                    ticker_info.exchange(),
                    Exchange::BinanceSpot | Exchange::BinanceLinear | Exchange::BinanceInverse
                );

                if is_binance {
                    let data_path = data::data_path(Some("market_data/binance/"));

                    log::debug!(
                        "request_fetch: Creating trades fetch task for {:?}, from={}, to={}",
                        ticker_info,
                        from_time,
                        to_time
                    );

                    let (task, handle) = Task::sip(
                        fetch_trades_batched(ticker_info, from_time, to_time, data_path),
                        move |batch| {
                            log::debug!(
                                "request_fetch: Received trades batch, count={}",
                                batch.len()
                            );
                            let data = FetchedData::Trades {
                                batch,
                                until_time: to_time,
                            };
                            Message::DistributeFetchedData {
                                layout_id,
                                pane_id,
                                data,
                                stream,
                            }
                        },
                        move |result| match result {
                            Ok(()) => {
                                log::debug!(
                                    "request_fetch: Trades fetch completed successfully for pane_id={}",
                                    pane_id
                                );
                                Message::ChangePaneStatus(pane_id, pane::Status::Ready)
                            }
                            Err(err) => {
                                log::error!(
                                    "request_fetch: Trades fetch failed for pane_id={}, error: {}",
                                    pane_id,
                                    err
                                );
                                Message::ErrorOccurred(
                                    Some(pane_id),
                                    DashboardError::Fetch(err.to_string()),
                                )
                            }
                        },
                    )
                    .abortable();

                    if let pane::Content::Kline { chart, .. } = &mut state.content
                        && let Some(c) = chart
                    {
                        c.set_handle(handle.abort_on_drop());
                    }

                    log::debug!("request_fetch: Trades fetch task created and returned");
                    return task;
                } else {
                    // 原程序逻辑：如果不是 Binance，直接返回 Task::none()，不进行任何转换
                    // 这样保持与原程序逻辑一致
                    log::debug!(
                        "request_fetch: Exchange {:?} is not Binance, trades fetch not supported (only Binance supports historical trades download)",
                        ticker_info.exchange()
                    );
                    // 重置 fetching_trades 标志，避免卡住
                    if let pane::Content::Kline { chart, .. } = &mut state.content {
                        if let Some(c) = chart {
                            c.reset_request_handler();
                        }
                    }
                }
            } else {
                log::debug!(
                    "request_fetch: No DepthAndTrades stream found for trades fetch"
                );
            }
        }
    }

    Task::none()
}

fn request_fetch_many(
    state: &mut pane::State,
    layout_id: uuid::Uuid,
    reqs: impl IntoIterator<Item = (uuid::Uuid, FetchRange, Option<StreamKind>)>,
) -> Task<Message> {
    let tasks = reqs
        .into_iter()
        .map(|(req_id, fetch, stream)| request_fetch(state, layout_id, req_id, fetch, stream))
        .collect::<Vec<_>>();
    Task::batch(tasks)
}

fn oi_fetch_task(
    layout_id: uuid::Uuid,
    pane_id: uuid::Uuid,
    stream: StreamKind,
    req_id: Option<uuid::Uuid>,
    range: Option<(u64, u64)>,
) -> Task<Message> {
    let update_status = Task::done(Message::ChangePaneStatus(
        pane_id,
        pane::Status::Loading(exchange::fetcher::InfoKind::FetchingOI),
    ));

    let fetch_task = match stream {
        StreamKind::Kline {
            ticker_info,
            timeframe,
        } => Task::perform(
            adapter::fetch_open_interest(ticker_info.ticker, timeframe, range)
                .map_err(|err| format!("{err}")),
            move |result| match result {
                Ok(oi) => {
                    let data = FetchedData::OI { data: oi, req_id };
                    Message::DistributeFetchedData {
                        layout_id,
                        pane_id,
                        data,
                        stream,
                    }
                }
                Err(err) => Message::ErrorOccurred(Some(pane_id), DashboardError::Fetch(err)),
            },
        ),
        _ => Task::none(),
    };

    update_status.chain(fetch_task)
}

fn kline_fetch_task(
    layout_id: uuid::Uuid,
    pane_id: uuid::Uuid,
    stream: StreamKind,
    req_id: Option<uuid::Uuid>,
    range: Option<(u64, u64)>,
) -> Task<Message> {
    let update_status = Task::done(Message::ChangePaneStatus(
        pane_id,
        pane::Status::Loading(exchange::fetcher::InfoKind::FetchingKlines),
    ));

    let fetch_task = match stream {
        StreamKind::Kline {
            ticker_info,
            timeframe,
        } => Task::perform(
            adapter::fetch_klines(ticker_info, timeframe, range)
                .map_err(|err| err.to_user_message()),
            move |result| match result {
                Ok(klines) => {
                    let data = FetchedData::Klines {
                        data: klines,
                        req_id,
                    };
                    Message::DistributeFetchedData {
                        layout_id,
                        pane_id,
                        data,
                        stream,
                    }
                }
                Err(err) => {
                    Message::ErrorOccurred(Some(pane_id), DashboardError::Fetch(err.to_string()))
                }
            },
        ),
        _ => Task::none(),
    };

    update_status.chain(fetch_task)
}

pub fn fetch_trades_batched(
    ticker_info: TickerInfo,
    from_time: u64,
    to_time: u64,
    data_path: PathBuf,
) -> impl Straw<(), Vec<Trade>, AdapterError> {
    sipper(async move |mut progress| {
        let mut latest_trade_t = from_time;

        while latest_trade_t < to_time {
            match binance::fetch_trades(ticker_info, latest_trade_t, data_path.clone()).await {
                Ok(batch) => {
                    if batch.is_empty() {
                        break;
                    }

                    latest_trade_t = batch.last().map_or(latest_trade_t, |trade| trade.time);

                    let () = progress.send(batch).await;
                }
                Err(err) => return Err(err),
            }
        }

        Ok(())
    })
}

pub fn depth_subscription(
    ticker_info: TickerInfo,
    tick_mlpt: Option<TickMultiplier>,
    push_freq: PushFrequency,
) -> Subscription<exchange::Event> {
    let exchange = ticker_info.exchange();

    let config = StreamConfig::new(ticker_info, exchange, tick_mlpt, push_freq);

    match exchange {
        Exchange::BinanceSpot | Exchange::BinanceInverse | Exchange::BinanceLinear => {
            let builder = |cfg: &StreamConfig<TickerInfo>| {
                binance::connect_market_stream(cfg.id, cfg.push_freq)
            };
            Subscription::run_with(config, builder)
        }
        Exchange::BybitSpot | Exchange::BybitLinear | Exchange::BybitInverse => {
            let builder = |cfg: &StreamConfig<TickerInfo>| {
                bybit::connect_market_stream(cfg.id, cfg.push_freq)
            };
            Subscription::run_with(config, builder)
        }
        Exchange::HyperliquidSpot | Exchange::HyperliquidLinear => {
            let builder = |cfg: &StreamConfig<TickerInfo>| {
                hyperliquid::connect_market_stream(cfg.id, cfg.tick_mltp, cfg.push_freq)
            };
            Subscription::run_with(config, builder)
        }
        Exchange::OkexLinear | Exchange::OkexInverse | Exchange::OkexSpot => {
            let builder =
                |cfg: &StreamConfig<TickerInfo>| okex::connect_market_stream(cfg.id, cfg.push_freq);
            Subscription::run_with(config, builder)
        }
    }
}

pub fn kline_subscription(
    exchange: Exchange,
    kline_subs: Vec<(TickerInfo, Timeframe)>,
) -> Subscription<exchange::Event> {
    let config = StreamConfig::new(kline_subs, exchange, None, PushFrequency::ServerDefault);
    match exchange {
        Exchange::BinanceSpot | Exchange::BinanceInverse | Exchange::BinanceLinear => {
            let builder = |cfg: &StreamConfig<Vec<(TickerInfo, Timeframe)>>| {
                binance::connect_kline_stream(cfg.id.clone(), cfg.market_type)
            };
            Subscription::run_with(config, builder)
        }
        Exchange::BybitSpot | Exchange::BybitInverse | Exchange::BybitLinear => {
            let builder = |cfg: &StreamConfig<Vec<(TickerInfo, Timeframe)>>| {
                bybit::connect_kline_stream(cfg.id.clone(), cfg.market_type)
            };
            Subscription::run_with(config, builder)
        }
        Exchange::HyperliquidSpot | Exchange::HyperliquidLinear => {
            let builder = |cfg: &StreamConfig<Vec<(TickerInfo, Timeframe)>>| {
                hyperliquid::connect_kline_stream(cfg.id.clone(), cfg.market_type)
            };
            Subscription::run_with(config, builder)
        }
        Exchange::OkexLinear | Exchange::OkexInverse | Exchange::OkexSpot => {
            let builder = |cfg: &StreamConfig<Vec<(TickerInfo, Timeframe)>>| {
                okex::connect_kline_stream(cfg.id.clone(), cfg.market_type)
            };
            Subscription::run_with(config, builder)
        }
    }
}
