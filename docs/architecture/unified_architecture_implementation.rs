//! 统一图表架构实现示例
//! 
//! 这是一个完整的实现示例，展示如何在实际代码中应用统一架构

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use uuid::Uuid;
use exchange::{TickerInfo, Trade, Kline, Depth};
use exchange::fetcher::{FetchRange, FetchedData};

// ============================================================================
// 1. 核心 Trait 定义
// ============================================================================

/// 图表基础接口
pub trait Chart: Send + Sync {
    type IndicatorKind;
    type Message;
    
    fn state(&self) -> &ViewState;
    fn mut_state(&mut self) -> &mut ViewState;
    fn invalidate_all(&mut self);
    fn is_empty(&self) -> bool;
}

/// 数据需求描述
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRequirements {
    pub needs_klines: bool,
    pub needs_trades: bool,
    pub needs_depth: bool,
    pub needs_open_interest: bool,
    pub supports_historical: bool,
    pub supports_tick_basis: bool,
}

impl Default for DataRequirements {
    fn default() -> Self {
        Self {
            needs_klines: false,
            needs_trades: false,
            needs_depth: false,
            needs_open_interest: false,
            supports_historical: false,
            supports_tick_basis: false,
        }
    }
}

/// 实时数据类型
#[derive(Debug, Clone)]
pub enum RealtimeData {
    DepthAndTrades {
        depth: Depth,
        trades: Vec<Trade>,
        time: u64,
    },
    Kline(Kline),
    // 未来可以扩展其他类型
}

/// 数据管理接口
pub trait ChartDataManager: Chart {
    /// 获取数据需求
    fn data_requirements(&self) -> DataRequirements;
    
    /// 获取订阅者 ID
    fn subscriber_id(&self) -> SubscriberId;
    
    /// 请求数据（可选，实时图表可能不需要）
    fn request_data(&mut self, range: FetchRange) -> Option<Action> {
        // 默认实现：不支持历史数据的图表返回 None
        None
    }
    
    /// 检查缺失数据（可选）
    fn missing_data_task(&mut self) -> Option<Action> {
        None
    }
    
    /// 插入实时数据（必须实现）
    fn insert_realtime_data(&mut self, data: &RealtimeData);
    
    /// 插入历史数据（可选）
    fn insert_historical_data(&mut self, data: Arc<FetchedData>) {
        // 默认实现：不支持历史数据的图表忽略
    }
}

// ============================================================================
// 2. 统一数据管理器
// ============================================================================

pub type SubscriberId = Uuid;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct DataKey {
    pub ticker: TickerInfo,
    pub range: FetchRange,
    pub basis: Basis,
}

#[derive(Debug, Clone)]
pub enum RequestResult {
    Cached(Arc<FetchedData>),
    Pending,
    NewRequest(DataKey),
}

/// 全局请求去重器
struct GlobalRequestDeduplicator {
    pending: Arc<RwLock<HashMap<DataKey, Vec<SubscriberId>>>>,
}

impl GlobalRequestDeduplicator {
    fn new() -> Self {
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    fn check_pending(&self, key: &DataKey) -> Option<Vec<SubscriberId>> {
        self.pending.read().unwrap().get(key).cloned()
    }
    
    fn register_request(&self, key: DataKey, subscriber: SubscriberId) {
        let mut pending = self.pending.write().unwrap();
        pending.entry(key).or_insert_with(Vec::new).push(subscriber);
    }
    
    fn get_subscribers(&self, key: &DataKey) -> Vec<SubscriberId> {
        self.pending.read().unwrap().get(key).cloned().unwrap_or_default()
    }
    
    fn remove_request(&self, key: &DataKey) {
        self.pending.write().unwrap().remove(key);
    }
}

/// 统一数据管理器
pub struct UnifiedDataManager {
    // 原始数据缓存
    raw_trades_cache: Arc<RwLock<HashMap<DataKey, Arc<Vec<Trade>>>>>,
    raw_klines_cache: Arc<RwLock<HashMap<DataKey, Arc<Vec<Kline>>>>>,
    
    // 聚合数据缓存（按类型）
    kline_aggregated_cache: Arc<RwLock<HashMap<DataKey, Arc<TimeSeries<KlineDataPoint>>>>>,
    heatmap_aggregated_cache: Arc<RwLock<HashMap<DataKey, Arc<TimeSeries<HeatmapDataPoint>>>>>,
    
    // 请求去重
    deduplicator: GlobalRequestDeduplicator,
    
    // 订阅者注册表
    subscribers: Arc<RwLock<HashMap<SubscriberId, DataRequirements>>>,
}

impl UnifiedDataManager {
    pub fn new() -> Self {
        Self {
            raw_trades_cache: Arc::new(RwLock::new(HashMap::new())),
            raw_klines_cache: Arc::new(RwLock::new(HashMap::new())),
            kline_aggregated_cache: Arc::new(RwLock::new(HashMap::new())),
            heatmap_aggregated_cache: Arc::new(RwLock::new(HashMap::new())),
            deduplicator: GlobalRequestDeduplicator::new(),
            subscribers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// 注册订阅者
    pub fn register_subscriber(&self, id: SubscriberId, requirements: DataRequirements) {
        self.subscribers.write().unwrap().insert(id, requirements);
    }
    
    /// 注销订阅者
    pub fn unregister_subscriber(&self, id: SubscriberId) {
        self.subscribers.write().unwrap().remove(&id);
    }
    
    /// 请求数据（统一入口）
    pub fn request_data(
        &self,
        key: DataKey,
        subscriber: SubscriberId,
        requirements: &DataRequirements,
    ) -> RequestResult {
        // 1. 检查缓存（根据需求类型）
        if requirements.needs_trades {
            if let Some(cached) = self.raw_trades_cache.read().unwrap().get(&key) {
                return RequestResult::Cached(Arc::new(FetchedData::Trades {
                    batch: (**cached).clone(),
                    until_time: key.range.end(),
                }));
            }
        }
        
        if requirements.needs_klines {
            if let Some(cached) = self.raw_klines_cache.read().unwrap().get(&key) {
                return RequestResult::Cached(Arc::new(FetchedData::Klines {
                    data: (**cached).clone(),
                    req_id: None,
                }));
            }
        }
        
        // 2. 检查是否有进行中的请求（全局去重）
        if let Some(subscribers) = self.deduplicator.check_pending(&key) {
            // 添加到订阅列表
            self.deduplicator.register_request(key.clone(), subscriber);
            return RequestResult::Pending;
        }
        
        // 3. 创建新请求
        self.deduplicator.register_request(key.clone(), subscriber);
        RequestResult::NewRequest(key)
    }
    
    /// 数据到达后更新缓存并通知订阅者
    pub fn on_data_fetched(&self, key: DataKey, data: FetchedData) -> Vec<SubscriberId> {
        // 1. 更新缓存
        match &data {
            FetchedData::Trades { batch, .. } => {
                self.raw_trades_cache.write().unwrap()
                    .insert(key.clone(), Arc::new(batch.clone()));
            }
            FetchedData::Klines { data, .. } => {
                self.raw_klines_cache.write().unwrap()
                    .insert(key.clone(), Arc::new(data.clone()));
            }
            _ => {}
        }
        
        // 2. 获取所有订阅者
        let subscribers = self.deduplicator.get_subscribers(&key);
        
        // 3. 移除请求记录
        self.deduplicator.remove_request(&key);
        
        subscribers
    }
}

// ============================================================================
// 3. 图表注册表
// ============================================================================

/// 图表类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChartType {
    Kline,      // Footprint + Candles
    Heatmap,
    Ladder,
    TimeAndSales,
    Comparison,
    // 未来可以扩展
}

/// 图表注册表
pub struct ChartRegistry {
    charts: HashMap<SubscriberId, Box<dyn ChartDataManager>>,
    charts_by_type: HashMap<ChartType, Vec<SubscriberId>>,
    data_manager: Arc<UnifiedDataManager>,
}

impl ChartRegistry {
    pub fn new(data_manager: Arc<UnifiedDataManager>) -> Self {
        Self {
            charts: HashMap::new(),
            charts_by_type: HashMap::new(),
            data_manager,
        }
    }
    
    /// 注册图表
    pub fn register_chart<C: ChartDataManager + 'static>(
        &mut self,
        chart: C,
        chart_type: ChartType,
    ) -> SubscriberId {
        let id = chart.subscriber_id();
        let requirements = chart.data_requirements();
        
        // 注册到数据管理器
        self.data_manager.register_subscriber(id, requirements.clone());
        
        // 添加到注册表
        self.charts.insert(id, Box::new(chart));
        self.charts_by_type.entry(chart_type).or_insert_with(Vec::new).push(id);
        
        id
    }
    
    /// 注销图表
    pub fn unregister_chart(&mut self, id: SubscriberId) {
        self.data_manager.unregister_subscriber(id);
        self.charts.remove(&id);
        
        // 从类型索引中移除
        for subscribers in self.charts_by_type.values_mut() {
            subscribers.retain(|&sid| sid != id);
        }
    }
    
    /// 获取图表（可变）
    pub fn get_chart_mut(&mut self, id: SubscriberId) -> Option<&mut dyn ChartDataManager> {
        self.charts.get_mut(&id).map(|c| c.as_mut())
    }
    
    /// 获取所有需要相同数据的图表
    pub fn get_charts_needing_data(&self, key: &DataKey) -> Vec<SubscriberId> {
        // 根据 key 查找所有需要该数据的图表
        // 简化实现
        self.charts.keys().copied().collect()
    }
}

// ============================================================================
// 4. 具体图表实现示例
// ============================================================================

// KlineChart 实现
impl ChartDataManager for KlineChart {
    fn data_requirements(&self) -> DataRequirements {
        let needs_trades = match &self.kind {
            KlineChartKind::Footprint { .. } => true,
            KlineChartKind::Candles { studies } => {
                studies.iter().any(|s| matches!(s, FootprintStudy::HVN { .. }))
            }
        };
        
        DataRequirements {
            needs_klines: true,
            needs_trades,
            needs_depth: false,
            needs_open_interest: false,
            supports_historical: true,
            supports_tick_basis: true,
        }
    }
    
    fn subscriber_id(&self) -> SubscriberId {
        // 可以使用图表的唯一 ID
        self.id  // 假设有 id 字段
    }
    
    fn request_data(&mut self, range: FetchRange) -> Option<Action> {
        let key = DataKey {
            ticker: self.chart.ticker_info,
            range,
            basis: self.chart.basis,
        };
        
        match self.data_manager.request_data(
            key.clone(),
            self.subscriber_id(),
            &self.data_requirements(),
        ) {
            RequestResult::Cached(data) => {
                self.insert_historical_data(data);
                None
            }
            RequestResult::NewRequest(key) => {
                Some(Action::RequestFetch(key))
            }
            RequestResult::Pending => None,
        }
    }
    
    fn insert_realtime_data(&mut self, data: &RealtimeData) {
        match data {
            RealtimeData::DepthAndTrades { trades, .. } => {
                self.insert_trades_buffer(trades);
            }
            RealtimeData::Kline(kline) => {
                self.update_latest_kline(kline);
            }
        }
    }
    
    fn insert_historical_data(&mut self, data: Arc<FetchedData>) {
        match data.as_ref() {
            FetchedData::Klines { data, req_id } => {
                if let Some(req_id) = req_id {
                    self.insert_hist_klines(*req_id, data);
                }
            }
            FetchedData::Trades { batch, .. } => {
                self.insert_raw_trades(batch.clone(), true);
            }
            _ => {}
        }
    }
}

// HeatmapChart 实现
impl ChartDataManager for HeatmapChart {
    fn data_requirements(&self) -> DataRequirements {
        DataRequirements {
            needs_klines: false,
            needs_trades: true,
            needs_depth: true,
            needs_open_interest: false,
            supports_historical: false,
            supports_tick_basis: false,
        }
    }
    
    fn subscriber_id(&self) -> SubscriberId {
        self.id  // 假设有 id 字段
    }
    
    fn insert_realtime_data(&mut self, data: &RealtimeData) {
        match data {
            RealtimeData::DepthAndTrades { depth, trades, time } => {
                self.insert_datapoint(trades, *time, depth);
            }
            _ => {}
        }
    }
}

// Ladder 实现
impl ChartDataManager for Ladder {
    fn data_requirements(&self) -> DataRequirements {
        DataRequirements {
            needs_klines: false,
            needs_trades: true,
            needs_depth: true,
            needs_open_interest: false,
            supports_historical: false,
            supports_tick_basis: false,
        }
    }
    
    fn subscriber_id(&self) -> SubscriberId {
        self.id  // 假设有 id 字段
    }
    
    fn insert_realtime_data(&mut self, data: &RealtimeData) {
        match data {
            RealtimeData::DepthAndTrades { depth, trades, time } => {
                self.insert_buffers(*time, depth, trades);
            }
            _ => {}
        }
    }
}

// ============================================================================
// 5. Dashboard 集成
// ============================================================================

pub struct Dashboard {
    data_manager: Arc<UnifiedDataManager>,
    chart_registry: ChartRegistry,
    // ... 其他字段
}

impl Dashboard {
    pub fn new() -> Self {
        let data_manager = Arc::new(UnifiedDataManager::new());
        let chart_registry = ChartRegistry::new(Arc::clone(&data_manager));
        
        Self {
            data_manager,
            chart_registry,
            // ...
        }
    }
    
    /// 创建图表时注册
    pub fn create_kline_chart(&mut self, chart: KlineChart) -> SubscriberId {
        self.chart_registry.register_chart(chart, ChartType::Kline)
    }
    
    pub fn create_heatmap_chart(&mut self, chart: HeatmapChart) -> SubscriberId {
        self.chart_registry.register_chart(chart, ChartType::Heatmap)
    }
    
    pub fn create_ladder(&mut self, ladder: Ladder) -> SubscriberId {
        self.chart_registry.register_chart(ladder, ChartType::Ladder)
    }
    
    /// 统一处理数据请求
    pub fn handle_data_request(&mut self, request: DataRequest) -> Task<Message> {
        let key = request.key;
        
        // 检查全局去重
        if let Some(subscribers) = self.data_manager.deduplicator.check_pending(&key) {
            // 请求已在进行，只需等待
            return Task::none();
        }
        
        // 创建新任务
        let data_manager = Arc::clone(&self.data_manager);
        let task = self.create_fetch_task(key.clone(), move |data| {
            // 数据到达后，通知所有订阅者
            let subscribers = data_manager.on_data_fetched(key.clone(), data);
            // 通过事件系统通知订阅者
            // ...
        });
        
        // 注册任务
        self.data_manager.deduplicator.register_request(key, request.subscriber);
        task
    }
    
    /// 统一处理实时数据
    pub fn handle_realtime_data(&mut self, stream: &StreamKind, data: RealtimeData) {
        // 分发给所有匹配的图表
        self.chart_registry.charts.iter_mut().for_each(|(_, chart)| {
            // 检查图表是否需要该数据
            let requirements = chart.data_requirements();
            let matches = match &data {
                RealtimeData::DepthAndTrades { .. } => {
                    requirements.needs_depth || requirements.needs_trades
                }
                RealtimeData::Kline(_) => requirements.needs_klines,
            };
            
            if matches {
                chart.insert_realtime_data(&data);
            }
        });
    }
}

// ============================================================================
// 6. 扩展新图表类型示例
// ============================================================================

/// 示例：新的 VolumeProfile 图表
pub struct VolumeProfileChart {
    id: SubscriberId,
    chart: ViewState,
    data_source: TimeSeries<VolumeProfileDataPoint>,
    data_manager: Arc<UnifiedDataManager>,
    // ...
}

impl Chart for VolumeProfileChart {
    type IndicatorKind = ();
    type Message = ();
    
    fn state(&self) -> &ViewState { &self.chart }
    fn mut_state(&mut self) -> &mut ViewState { &mut self.chart }
    fn invalidate_all(&mut self) { /* ... */ }
    fn is_empty(&self) -> bool { self.data_source.datapoints.is_empty() }
}

impl ChartDataManager for VolumeProfileChart {
    fn data_requirements(&self) -> DataRequirements {
        DataRequirements {
            needs_klines: true,
            needs_trades: true,
            needs_depth: false,
            needs_open_interest: false,
            supports_historical: true,
            supports_tick_basis: true,
        }
    }
    
    fn subscriber_id(&self) -> SubscriberId {
        self.id
    }
    
    fn request_data(&mut self, range: FetchRange) -> Option<Action> {
        // 实现数据请求逻辑
        // ...
        None
    }
    
    fn insert_realtime_data(&mut self, data: &RealtimeData) {
        // 实现实时数据插入
        // ...
    }
    
    fn insert_historical_data(&mut self, data: Arc<FetchedData>) {
        // 实现历史数据插入
        // ...
    }
}

// 使用：只需注册即可
// dashboard.create_volume_profile_chart(VolumeProfileChart::new(...));

