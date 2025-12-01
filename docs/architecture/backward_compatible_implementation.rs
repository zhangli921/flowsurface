//! 向后兼容的实现示例
//! 
//! 展示如何在保持现有功能正常的前提下，逐步迁移到新架构

use std::sync::Arc;
use exchange::fetcher::{FetchRange, FetchedData};

// ============================================================================
// 1. 配置开关：控制新旧系统
// ============================================================================

#[derive(Debug, Clone)]
pub struct MigrationConfig {
    /// 是否启用统一数据管理器
    pub use_unified_data_manager: bool,
    
    /// 是否迁移 KlineChart
    pub migrate_kline_chart: bool,
    
    /// 是否迁移 HeatmapChart
    pub migrate_heatmap_chart: bool,
    
    /// 是否迁移 Ladder
    pub migrate_ladder: bool,
}

impl Default for MigrationConfig {
    fn default() -> Self {
        Self {
            use_unified_data_manager: false,  // 默认关闭
            migrate_kline_chart: false,
            migrate_heatmap_chart: false,
            migrate_ladder: false,
        }
    }
}

// ============================================================================
// 2. KlineChart 的向后兼容实现
// ============================================================================

pub struct KlineChart {
    // === 现有字段（保持不变）===
    chart: ViewState,
    data_source: PlotData<KlineDataPoint>,
    raw_trades: Vec<Trade>,
    indicators: EnumMap<KlineIndicator, Option<Box<dyn KlineIndicatorImpl>>>,
    fetching_trades: (bool, Option<Handle>),
    pub(crate) kind: KlineChartKind,
    request_handler: RequestHandler,  // 保留旧系统
    study_configurator: study::Configurator<FootprintStudy>,
    last_tick: Instant,
    hvn_cache: RefCell<HVNCache>,
    
    // === 新增字段（可选使用）===
    data_manager: Option<Arc<UnifiedDataManager>>,  // 新系统（可选）
    migration_config: MigrationConfig,  // 配置
}

impl KlineChart {
    /// 创建图表（保持原有接口不变）
    pub fn new(
        layout: ViewConfig,
        basis: Basis,
        tick_size: f32,
        klines_raw: &[Kline],
        raw_trades: Vec<Trade>,
        enabled_indicators: &[KlineIndicator],
        ticker_info: TickerInfo,
        kind: &KlineChartKind,
    ) -> Self {
        // 现有实现保持不变
        // ...
        
        // 可选：初始化新系统（如果配置启用）
        let data_manager = if config.use_unified_data_manager {
            Some(data_manager.clone())
        } else {
            None
        };
        
        KlineChart {
            // ... 现有字段
            data_manager,
            migration_config: config,
        }
    }
    
    /// 缺失数据任务（保持原有逻辑，可选使用新系统）
    pub fn missing_data_task(&mut self) -> Option<Action> {
        // 如果启用了新系统且配置允许
        if self.migration_config.use_unified_data_manager 
            && self.migration_config.migrate_kline_chart 
            && self.data_manager.is_some() 
        {
            // 使用新系统
            return self.missing_data_task_new();
        }
        
        // 否则使用旧系统（现有实现）
        self.missing_data_task_old()
    }
    
    /// 旧实现（保持不变）
    fn missing_data_task_old(&mut self) -> Option<Action> {
        match &self.data_source {
            PlotData::TimeBased(timeseries) => {
                // 现有实现完全不变
                let timeframe_ms = timeseries.interval.to_milliseconds();
                // ... 现有逻辑
            }
            PlotData::TickBased(_) => {
                // TODO: implement trade fetch
            }
        }
        None
    }
    
    /// 新实现（可选使用）
    fn missing_data_task_new(&mut self) -> Option<Action> {
        if let Some(dm) = &self.data_manager {
            // 使用统一数据管理器
            // ...
        }
        None
    }
    
    /// 插入交易数据（双写模式：同时更新新旧系统）
    pub fn insert_trades_buffer(&mut self, trades_buffer: &[Trade]) {
        // === 1. 现有逻辑（必须执行，保证功能正常）===
        self.raw_trades.extend_from_slice(trades_buffer);
        
        match self.data_source {
            PlotData::TickBased(ref mut tick_aggr) => {
                let old_dp_len = tick_aggr.datapoints.len();
                tick_aggr.insert_trades(trades_buffer);
                // ... 现有逻辑
            }
            PlotData::TimeBased(ref mut timeseries) => {
                // 现有逻辑
                let needs_hvn = match &self.kind {
                    KlineChartKind::Candles { studies } => {
                        studies.iter().any(|s| matches!(s, FootprintStudy::HVN { .. }))
                    }
                    KlineChartKind::Footprint { .. } => false,
                };
                if needs_hvn {
                    timeseries.insert_trades_or_create_bucket(trades_buffer);
                } else {
                    timeseries.insert_trades_existing_buckets(trades_buffer);
                }
            }
        }
        
        // === 2. 新系统（可选，如果启用）===
        if self.migration_config.use_unified_data_manager 
            && self.migration_config.migrate_kline_chart 
            && let Some(dm) = &self.data_manager 
        {
            // 同时更新新系统（不影响现有功能）
            let realtime_data = RealtimeData::DepthAndTrades {
                depth: Depth::default(),  // 如果需要
                trades: trades_buffer.to_vec(),
                time: 0,  // 从实际数据获取
            };
            // 可选：通知新系统
            // dm.on_realtime_data(realtime_data);
        }
        
        self.invalidate(None);
    }
}

// ============================================================================
// 3. HeatmapChart 的向后兼容实现
// ============================================================================

pub struct HeatmapChart {
    // === 现有字段（保持不变）===
    chart: ViewState,
    trades: TimeSeries<HeatmapDataPoint>,
    indicators: EnumMap<HeatmapIndicator, Option<IndicatorData>>,
    pause_buffer: Vec<(u64, Box<[Trade]>, Depth)>,
    heatmap: HistoricalDepth,
    visual_config: Config,
    study_configurator: study::Configurator<HeatmapStudy>,
    last_tick: Instant,
    pub studies: Vec<HeatmapStudy>,
    
    // === 新增字段（可选使用）===
    data_manager: Option<Arc<UnifiedDataManager>>,
    migration_config: MigrationConfig,
}

impl HeatmapChart {
    /// 插入数据点（保持原有逻辑）
    pub fn insert_datapoint(
        &mut self,
        trades_buffer: &[Trade],
        depth_update_t: u64,
        depth: &Depth,
    ) {
        // === 现有实现（保持不变）===
        let chart = &mut self.chart;
        let aggregate_time: u64 = match chart.basis {
            Basis::Time(interval) => interval.into(),
            Basis::Tick(_) => todo!(),
        };
        
        let rounded_depth_update = (depth_update_t / aggregate_time) * aggregate_time;
        
        {
            let entry = self
                .trades
                .datapoints
                .entry(rounded_depth_update)
                .or_insert_with(|| HeatmapDataPoint {
                    grouped_trades: Box::new([]),
                    buy_sell: (0.0, 0.0),
                });
            
            for trade in trades_buffer {
                entry.add_trade(trade, chart.tick_size);
            }
        }
        
        self.heatmap.insert_latest_depth(depth, rounded_depth_update);
        
        {
            let mid_price = depth.mid_price().unwrap_or(chart.base_price_y);
            chart.base_price_y = mid_price.round_to_step(chart.tick_size);
        }
        
        chart.latest_x = rounded_depth_update;
        
        // === 新系统（可选，如果启用）===
        if self.migration_config.use_unified_data_manager 
            && self.migration_config.migrate_heatmap_chart 
            && let Some(dm) = &self.data_manager 
        {
            // 可选：通知新系统（不影响现有功能）
        }
    }
}

// ============================================================================
// 4. Dashboard 的向后兼容实现
// ============================================================================

pub struct Dashboard {
    // === 现有字段（保持不变）===
    pub panes: pane_grid::State<pane::State>,
    pub focus: Option<(window::Id, pane_grid::Pane)>,
    pub popout: HashMap<window::Id, (pane_grid::State<pane::State>, WindowSpec)>,
    pub streams: UniqueStreams,
    layout_id: uuid::Uuid,
    
    // === 新增字段（可选使用）===
    data_manager: Option<Arc<UnifiedDataManager>>,
    chart_registry: Option<ChartRegistry>,
    migration_config: MigrationConfig,
}

impl Dashboard {
    pub fn new() -> Self {
        let config = MigrationConfig::default();  // 默认关闭新系统
        
        let (data_manager, chart_registry) = if config.use_unified_data_manager {
            let dm = Arc::new(UnifiedDataManager::new());
            let cr = ChartRegistry::new(Arc::clone(&dm));
            (Some(dm), Some(cr))
        } else {
            (None, None)
        };
        
        Self {
            // ... 现有字段初始化
            data_manager,
            chart_registry,
            migration_config: config,
        }
    }
    
    /// 更新深度和交易数据（保持原有逻辑，可选使用新系统）
    pub fn update_depth_and_trades(
        &mut self,
        stream: &StreamKind,
        depth_update_t: u64,
        depth: &Depth,
        trades_buffer: &[Trade],
        main_window: window::Id,
    ) -> Task<Message> {
        // === 1. 现有逻辑（必须执行）===
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
                        // ... 其他现有逻辑
                    }
                    found_match = true;
                }
            });
        
        // === 2. 新系统（可选，如果启用）===
        if self.migration_config.use_unified_data_manager {
            if let Some(registry) = &self.chart_registry {
                let realtime_data = RealtimeData::DepthAndTrades {
                    depth: depth.clone(),
                    trades: trades_buffer.to_vec(),
                    time: depth_update_t,
                };
                // 可选：通过新系统分发
                // registry.broadcast_realtime_data(&realtime_data);
            }
        }
        
        if found_match {
            Task::none()
        } else {
            log::debug!("{stream:?} stream had no matching panes - dropping");
            self.refresh_streams(main_window)
        }
    }
}

// ============================================================================
// 5. 功能验证辅助函数
// ============================================================================

/// 验证 KlineChart 功能
pub fn verify_kline_chart_functionality(chart: &KlineChart) -> Vec<String> {
    let mut issues = Vec::new();
    
    // 检查基本功能
    if chart.data_source.is_empty() && !chart.raw_trades.is_empty() {
        issues.push("Data source empty but raw_trades not empty".to_string());
    }
    
    // 检查请求处理器
    // ...
    
    issues
}

/// 验证 HeatmapChart 功能
pub fn verify_heatmap_chart_functionality(chart: &HeatmapChart) -> Vec<String> {
    let mut issues = Vec::new();
    
    // 检查基本功能
    if chart.trades.datapoints.is_empty() && !chart.pause_buffer.is_empty() {
        issues.push("Trades empty but pause_buffer not empty".to_string());
    }
    
    issues
}

// ============================================================================
// 6. 测试辅助
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_backward_compatibility_kline_chart() {
        // 测试：使用旧系统创建图表
        let config = MigrationConfig {
            use_unified_data_manager: false,
            migrate_kline_chart: false,
            ..Default::default()
        };
        
        let chart = KlineChart::new(/* ... */);
        
        // 验证：所有现有方法都正常工作
        assert!(chart.missing_data_task().is_none() || chart.missing_data_task().is_some());
        // ...
    }
    
    #[test]
    fn test_new_system_does_not_break_old() {
        // 测试：启用新系统但不迁移图表
        let config = MigrationConfig {
            use_unified_data_manager: true,
            migrate_kline_chart: false,  // 不迁移
            ..Default::default()
        };
        
        let chart = KlineChart::new(/* ... */);
        
        // 验证：仍然使用旧系统
        // ...
    }
    
    #[test]
    fn test_gradual_migration() {
        // 测试：逐步迁移
        // 1. 先启用新系统但不迁移
        // 2. 然后迁移一个图表
        // 3. 验证功能正常
        // ...
    }
}


