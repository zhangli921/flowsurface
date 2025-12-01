# 统一图表架构设计（可扩展）

## 设计目标

1. **统一接口**：所有图表类型实现统一的接口
2. **可扩展性**：新图表类型可以轻松接入
3. **数据共享**：最大化数据共享，减少重复
4. **类型安全**：利用 Rust 类型系统保证安全性
5. **向后兼容**：现有代码可以逐步迁移

## 架构层次

```
┌─────────────────────────────────────────────────────────────┐
│                    Dashboard Layer                          │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  UnifiedDataManager: 全局数据管理器                    │  │
│  │  └─ 统一处理所有图表的数据请求和缓存                    │  │
│  └──────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  ChartRegistry: 图表注册表                            │  │
│  │  └─ 管理所有图表实例和生命周期                          │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────────┐
│                    Chart Trait Layer                        │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  Chart: 基础图表接口                                  │  │
│  │  ChartDataManager: 数据管理接口                       │  │
│  │  ChartRenderer: 渲染接口                              │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────────┐
│              Chart Implementation Layer                     │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐   │
│  │KlineChart│  │Heatmap   │  │Ladder   │  │Future... │   │
│  │          │  │Chart     │  │         │  │          │   │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘   │
└─────────────────────────────────────────────────────────────┘
```

## 核心设计

### 1. 统一图表 Trait

```rust
/// 图表基础接口
pub trait Chart: Send + Sync {
    type IndicatorKind;
    type Message;
    
    fn state(&self) -> &ViewState;
    fn mut_state(&mut self) -> &mut ViewState;
    fn invalidate_all(&mut self);
    fn is_empty(&self) -> bool;
}

/// 数据管理接口
pub trait ChartDataManager: Chart {
    /// 数据需求描述
    fn data_requirements(&self) -> DataRequirements;
    
    /// 请求数据
    fn request_data(&mut self, range: FetchRange) -> Option<Action>;
    
    /// 检查缺失数据
    fn missing_data_task(&mut self) -> Option<Action>;
    
    /// 插入实时数据
    fn insert_realtime_data(&mut self, data: &RealtimeData);
    
    /// 插入历史数据
    fn insert_historical_data(&mut self, data: FetchedData);
    
    /// 获取数据订阅 ID
    fn subscriber_id(&self) -> SubscriberId;
}

/// 数据需求描述
#[derive(Debug, Clone)]
pub struct DataRequirements {
    pub needs_klines: bool,
    pub needs_trades: bool,
    pub needs_depth: bool,
    pub needs_open_interest: bool,
    pub supports_historical: bool,  // 是否支持历史数据
    pub supports_tick_basis: bool,   // 是否支持 Tick-based
}
```

### 2. 数据管理策略模式

```rust
/// 数据管理策略
pub enum DataManagementStrategy {
    /// 完整数据管理（KlineChart）
    Full {
        request_handler: RequestHandler,
        raw_trades: Vec<Trade>,
        fetching_trades: (bool, Option<Handle>),
    },
    /// 实时数据管理（Heatmap, Ladder）
    RealtimeOnly {
        // 无额外字段，仅接收实时流
    },
    /// 混合模式（未来可能的新图表）
    Hybrid {
        request_handler: RequestHandler,
        // 其他字段
    },
}

/// 图表数据管理器实现
pub struct ChartDataManagerImpl {
    strategy: DataManagementStrategy,
    requirements: DataRequirements,
    subscriber_id: SubscriberId,
    data_manager: Arc<UnifiedDataManager>,
}

impl ChartDataManager for ChartDataManagerImpl {
    fn data_requirements(&self) -> DataRequirements {
        self.requirements.clone()
    }
    
    fn request_data(&mut self, range: FetchRange) -> Option<Action> {
        match &mut self.strategy {
            DataManagementStrategy::Full { request_handler, .. } => {
                request_fetch(request_handler, range)
            }
            DataManagementStrategy::RealtimeOnly => {
                // 实时图表不需要历史数据请求
                None
            }
            DataManagementStrategy::Hybrid { request_handler, .. } => {
                request_fetch(request_handler, range)
            }
        }
    }
    
    fn missing_data_task(&mut self) -> Option<Action> {
        if !self.requirements.supports_historical {
            return None;
        }
        
        match &mut self.strategy {
            DataManagementStrategy::Full { .. } => {
                // 实现缺失数据检查逻辑
                // ...
            }
            _ => None,
        }
    }
    
    fn insert_realtime_data(&mut self, data: &RealtimeData) {
        // 统一处理实时数据
    }
    
    fn insert_historical_data(&mut self, data: FetchedData) {
        if !self.requirements.supports_historical {
            return;
        }
        // 处理历史数据
    }
}
```

### 3. 统一数据管理器

```rust
/// 统一数据管理器（全局单例）
pub struct UnifiedDataManager {
    // 原始数据缓存
    raw_data_cache: RawDataCache,
    
    // 聚合数据缓存（按类型）
    aggregated_caches: HashMap<AggregationType, Box<dyn AggregatedCache>>,
    
    // 请求去重器
    request_deduplicator: GlobalRequestDeduplicator,
    
    // 事件总线
    event_bus: EventBus,
}

impl UnifiedDataManager {
    /// 请求数据（统一入口）
    pub fn request_data(
        &self,
        key: DataKey,
        subscriber: SubscriberId,
        requirements: &DataRequirements,
    ) -> RequestResult {
        // 1. 检查缓存
        if let Some(cached) = self.check_cache(&key, requirements) {
            return RequestResult::Cached(cached);
        }
        
        // 2. 检查是否有进行中的请求（全局去重）
        if let Some(existing_request) = self.request_deduplicator.check_pending(&key) {
            // 添加到订阅列表
            self.request_deduplicator.add_subscriber(&key, subscriber);
            return RequestResult::Pending;
        }
        
        // 3. 创建新请求
        self.request_deduplicator.register_request(key.clone(), subscriber);
        RequestResult::NewRequest(key)
    }
    
    /// 数据到达后通知所有订阅者
    pub fn on_data_fetched(&self, key: DataKey, data: FetchedData) {
        // 1. 更新缓存
        self.update_cache(&key, &data);
        
        // 2. 获取所有订阅者
        let subscribers = self.request_deduplicator.get_subscribers(&key);
        
        // 3. 通过事件总线通知
        self.event_bus.broadcast(DataEvent::Fetched {
            key,
            data: Arc::new(data),
            subscribers,
        });
    }
}
```

### 4. 图表注册表

```rust
/// 图表注册表：管理所有图表实例
pub struct ChartRegistry {
    charts: HashMap<SubscriberId, Box<dyn ChartDataManager>>,
    charts_by_type: HashMap<ChartType, Vec<SubscriberId>>,
    data_manager: Arc<UnifiedDataManager>,
}

impl ChartRegistry {
    /// 注册新图表
    pub fn register_chart<C: ChartDataManager + 'static>(
        &mut self,
        chart: C,
    ) -> SubscriberId {
        let id = chart.subscriber_id();
        let requirements = chart.data_requirements();
        
        // 注册到数据管理器
        self.data_manager.register_subscriber(id, requirements);
        
        // 添加到注册表
        self.charts.insert(id, Box::new(chart));
        
        id
    }
    
    /// 注销图表
    pub fn unregister_chart(&mut self, id: SubscriberId) {
        self.data_manager.unregister_subscriber(id);
        self.charts.remove(&id);
    }
    
    /// 获取所有需要相同数据的图表
    pub fn get_charts_needing_data(&self, key: &DataKey) -> Vec<SubscriberId> {
        // 查找所有需要该数据的图表
        // ...
    }
}
```

## 具体实现

### KlineChart 集成

```rust
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
    
    fn request_data(&mut self, range: FetchRange) -> Option<Action> {
        // 使用统一数据管理器
        let key = DataKey {
            ticker: self.chart.ticker_info,
            range,
            basis: self.chart.basis,
        };
        
        match self.data_manager.request_data(key, self.id, &self.data_requirements()) {
            RequestResult::Cached(data) => {
                self.insert_historical_data(data);
                None
            }
            RequestResult::NewRequest(key) => {
                // 发起请求
                Some(Action::RequestFetch(key))
            }
            RequestResult::Pending => None,
        }
    }
}
```

### HeatmapChart 集成

```rust
impl ChartDataManager for HeatmapChart {
    fn data_requirements(&self) -> DataRequirements {
        DataRequirements {
            needs_klines: false,
            needs_trades: true,
            needs_depth: true,
            needs_open_interest: false,
            supports_historical: false,  // Heatmap 不支持历史数据
            supports_tick_basis: false,
        }
    }
    
    fn request_data(&mut self, _range: FetchRange) -> Option<Action> {
        // Heatmap 不需要历史数据请求
        None
    }
    
    fn missing_data_task(&mut self) -> Option<Action> {
        // Heatmap 不需要主动请求数据
        None
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
```

### Ladder 集成

```rust
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
    
    fn insert_realtime_data(&mut self, data: &RealtimeData) {
        match data {
            RealtimeData::DepthAndTrades { depth, trades, time } => {
                self.insert_buffers(*time, depth, trades);
            }
            _ => {}
        }
    }
}
```

## Dashboard 层统一处理

```rust
impl Dashboard {
    pub fn new() -> Self {
        Self {
            data_manager: Arc::new(UnifiedDataManager::new()),
            chart_registry: ChartRegistry::new(Arc::clone(&data_manager)),
            // ...
        }
    }
    
    /// 统一处理数据请求
    pub fn handle_data_request(&mut self, request: DataRequest) -> Task<Message> {
        let key = request.key;
        
        // 检查全局去重
        if let Some(existing_task) = self.data_manager.get_pending_task(&key) {
            // 复用现有任务
            return existing_task;
        }
        
        // 创建新任务
        let task = self.create_fetch_task(key.clone());
        self.data_manager.register_task(key, task.clone());
        task
    }
    
    /// 数据到达后统一分发
    pub fn on_data_fetched(&mut self, key: DataKey, data: FetchedData) {
        // 1. 更新全局缓存
        self.data_manager.on_data_fetched(key.clone(), data.clone());
        
        // 2. 获取所有订阅者
        let subscribers = self.data_manager.get_subscribers(&key);
        
        // 3. 通知所有相关图表
        for subscriber_id in subscribers {
            if let Some(chart) = self.chart_registry.get_chart_mut(subscriber_id) {
                chart.insert_historical_data(data.clone());
            }
        }
    }
}
```

## 扩展新图表类型

### 示例：添加新的 VolumeProfile 图表

```rust
// 1. 定义图表结构
pub struct VolumeProfileChart {
    chart: ViewState,
    data_manager: ChartDataManagerImpl,
    // ... 其他字段
}

// 2. 实现 ChartDataManager trait
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
    
    // 实现其他方法...
}

// 3. 注册到 Dashboard
// Dashboard 会自动处理数据请求和分发
```

## 优势总结

| 特性 | 当前架构 | 新架构 |
|------|---------|--------|
| **统一接口** | ❌ 各图表独立 | ✅ 统一 trait |
| **数据共享** | ❌ 实例内去重 | ✅ 全局去重和共享 |
| **扩展性** | ❌ 需要修改多处 | ✅ 实现 trait 即可 |
| **类型安全** | ⚠️ 部分 | ✅ 完全类型安全 |
| **代码复用** | ❌ 重复代码多 | ✅ 高度复用 |

## 迁移路径

1. **阶段 1**：定义 trait 和统一数据管理器
2. **阶段 2**：为现有图表实现 trait（保持向后兼容）
3. **阶段 3**：逐步迁移到新架构
4. **阶段 4**：移除旧代码

## 总结

这个架构设计：
- ✅ **统一接口**：所有图表实现相同的 trait
- ✅ **可扩展**：新图表只需实现 trait
- ✅ **数据共享**：全局数据管理和缓存
- ✅ **类型安全**：Rust 类型系统保证
- ✅ **向后兼容**：可以逐步迁移

