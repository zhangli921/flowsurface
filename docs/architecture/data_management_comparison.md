# 图表数据管理架构对比

## 当前架构分析

### 1. KlineChart (Footprint/Candles)

**数据结构**：
```rust
pub struct KlineChart {
    chart: ViewState,
    data_source: PlotData<KlineDataPoint>,
    raw_trades: Vec<Trade>,                    // ✅ 存储原始交易数据
    indicators: EnumMap<...>,
    fetching_trades: (bool, Option<Handle>),   // ✅ 跟踪下载状态
    request_handler: RequestHandler,           // ✅ 请求管理器
    study_configurator: ...,
    last_tick: Instant,
    hvn_cache: RefCell<HVNCache>,
}
```

**数据管理机制**：
- ✅ **主动数据请求**：`missing_data_task()` 方法检查并请求缺失数据
- ✅ **请求去重**：`RequestHandler` 在实例内去重
- ✅ **状态跟踪**：`fetching_trades` 跟踪下载状态
- ✅ **原始数据存储**：`raw_trades` 存储所有交易数据
- ✅ **历史数据支持**：可以请求和下载历史数据

**数据流**：
```
missing_data_task() 
  └─> request_fetch() 
      └─> RequestHandler::add_request()
          └─> Dashboard::request_fetch()
              └─> 下载数据
                  └─> insert_raw_trades() / insert_hist_klines()
```

### 2. HeatmapChart

**数据结构**：
```rust
pub struct HeatmapChart {
    chart: ViewState,
    trades: TimeSeries<HeatmapDataPoint>,      // ❌ 没有 raw_trades
    indicators: EnumMap<...>,
    pause_buffer: Vec<(u64, Box<[Trade]>, Depth)>,
    heatmap: HistoricalDepth,
    visual_config: Config,
    study_configurator: ...,
    last_tick: Instant,
    studies: Vec<HeatmapStudy>,
    // ❌ 没有 request_handler
    // ❌ 没有 fetching_trades
}
```

**数据管理机制**：
- ❌ **无主动数据请求**：没有 `missing_data_task()` 方法
- ❌ **无请求管理器**：没有 `RequestHandler`
- ❌ **无状态跟踪**：没有 `fetching_trades`
- ❌ **无原始数据存储**：没有 `raw_trades`
- ⚠️ **仅实时数据**：通过 `insert_datapoint()` 接收实时流数据

**数据流**：
```
实时数据流
  └─> Dashboard::update_depth_and_trades()
      └─> HeatmapChart::insert_datapoint()
          └─> 直接插入到 TimeSeries<HeatmapDataPoint>
```

## 架构差异对比

| 特性 | KlineChart | HeatmapChart | 说明 |
|------|-----------|-------------|------|
| **RequestHandler** | ✅ 有 | ❌ 无 | KlineChart 可以主动请求数据 |
| **raw_trades** | ✅ 有 | ❌ 无 | KlineChart 存储原始数据 |
| **fetching_trades** | ✅ 有 | ❌ 无 | KlineChart 跟踪下载状态 |
| **missing_data_task** | ✅ 有 | ❌ 无 | KlineChart 主动检查缺失数据 |
| **历史数据支持** | ✅ 支持 | ❌ 不支持 | Heatmap 只处理实时数据 |
| **数据请求去重** | ✅ 实例内去重 | ❌ 无 | KlineChart 有去重机制 |
| **数据来源** | 实时 + 历史 | 仅实时 | 架构差异的根本原因 |

## 问题分析

### 为什么 Heatmap 没有数据请求机制？

1. **设计理念不同**：
   - KlineChart：需要历史数据来显示完整的 K 线图
   - Heatmap：主要显示实时订单簿深度，历史数据需求较少

2. **数据特性不同**：
   - KlineChart：可以批量下载历史 K 线和交易数据
   - Heatmap：需要实时订单簿深度，历史深度数据难以获取

3. **实现复杂度**：
   - KlineChart：相对简单，数据格式统一
   - Heatmap：需要处理订单簿快照，数据格式复杂

## 统一架构设计建议

### 方案：统一数据管理接口

```rust
// 1. 定义统一的数据管理 trait
pub trait ChartDataManager {
    type DataPoint: DataPoint;
    
    /// 请求数据
    fn request_data(&mut self, range: FetchRange) -> Option<Action>;
    
    /// 插入实时数据
    fn insert_realtime_data(&mut self, data: &[Trade]);
    
    /// 插入历史数据
    fn insert_historical_data(&mut self, data: FetchedData);
    
    /// 检查缺失数据
    fn missing_data_task(&mut self) -> Option<Action>;
}

// 2. KlineChart 实现
impl ChartDataManager for KlineChart {
    type DataPoint = KlineDataPoint;
    
    fn request_data(&mut self, range: FetchRange) -> Option<Action> {
        request_fetch(&mut self.request_handler, range)
    }
    
    fn missing_data_task(&mut self) -> Option<Action> {
        // 现有实现
    }
}

// 3. HeatmapChart 实现（可选）
impl ChartDataManager for HeatmapChart {
    type DataPoint = HeatmapDataPoint;
    
    fn request_data(&mut self, _range: FetchRange) -> Option<Action> {
        // Heatmap 可能不需要历史数据请求
        None
    }
    
    fn missing_data_task(&mut self) -> Option<Action> {
        // Heatmap 可能不需要主动请求
        None
    }
    
    fn insert_realtime_data(&mut self, trades: &[Trade]) {
        // 现有实现
    }
}
```

### 统一数据管理器

```rust
pub struct UnifiedChartDataManager {
    // 所有图表共享的数据管理器
    data_source: Arc<DataSourceManager>,
}

impl UnifiedChartDataManager {
    pub fn request_for_chart<C: ChartDataManager>(
        &self,
        chart: &mut C,
        range: FetchRange,
    ) -> Option<Action> {
        // 统一处理请求去重和缓存
        chart.request_data(range)
    }
}
```

## 总结

**答案：不是，它们使用不同的数据管理架构**

1. **KlineChart (Footprint/Candles)**：
   - 完整的数据管理机制
   - 支持历史数据请求
   - 有请求去重和状态跟踪

2. **HeatmapChart**：
   - 简化的数据管理
   - 仅处理实时数据
   - 无历史数据请求机制

**建议**：
- 如果需要统一架构，可以：
  1. 为所有图表定义统一的 `ChartDataManager` trait
  2. 实现统一的数据管理器
  3. 保持向后兼容，允许不同图表有不同的实现


