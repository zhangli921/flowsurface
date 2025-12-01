# 统一数据共享架构设计

## 图表类型分析

### 1. 数据结构对比

| 图表类型 | 数据结构 | 数据点类型 | 底层数据源 |
|---------|---------|-----------|-----------|
| **Footprint** | `PlotData<KlineDataPoint>` | `KlineDataPoint` | `Trade` + `Kline` |
| **Candles** | `PlotData<KlineDataPoint>` | `KlineDataPoint` | `Trade` + `Kline` |
| **Heatmap** | `TimeSeries<HeatmapDataPoint>` | `HeatmapDataPoint` | `Trade` + `Depth` |

### 2. 关键发现

✅ **Footprint 和 Candles 可以完全共享**
- 使用相同的数据结构：`PlotData<KlineDataPoint>`
- 相同的数据点类型：`KlineDataPoint { kline, footprint }`
- 可以共享同一份 `TimeSeries<KlineDataPoint>`

⚠️ **Heatmap 需要特殊处理**
- 不同的数据结构：`TimeSeries<HeatmapDataPoint>`
- 不同的数据点类型：`HeatmapDataPoint { grouped_trades, buy_sell }`
- 但底层原始数据（`Trade`）可以共享

## 分层共享架构

```
┌─────────────────────────────────────────────────────────────┐
│              原始数据层（Raw Data Layer）                     │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  TradeCache: 原始交易数据                             │  │
│  │  └─ Arc<Vec<Trade>>                                  │  │
│  └──────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  KlineCache: K线数据                                  │  │
│  │  └─ Arc<Vec<Kline>>                                  │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────────┐
│           聚合数据层（Aggregated Data Layer）                │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  KlineDataCache: KlineDataPoint 聚合数据              │  │
│  │  └─ Arc<TimeSeries<KlineDataPoint>>                  │  │
│  │     └─ 供 Footprint 和 Candles 共享                  │  │
│  └──────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  HeatmapDataCache: HeatmapDataPoint 聚合数据          │  │
│  │  └─ Arc<TimeSeries<HeatmapDataPoint>>               │  │
│  │     └─ 供 Heatmap 使用                                │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────────┐
│              视图层（View Layer）                           │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                │
│  │Footprint │  │ Candles  │  │ Heatmap  │                │
│  │  Chart   │  │  Chart   │  │  Chart   │                │
│  └──────────┘  └──────────┘  └──────────┘                │
└─────────────────────────────────────────────────────────────┘
```

## 实现方案

### 方案 A: 分层缓存（推荐）

**核心思想**：
1. **原始数据层**：共享 `Trade` 和 `Kline` 原始数据
2. **聚合数据层**：按图表类型分别缓存聚合后的数据
3. **视图层**：图表引用聚合数据

**优点**：
- Footprint 和 Candles 完全共享（相同聚合数据）
- Heatmap 独立缓存（不同聚合方式）
- 原始数据共享，减少内存占用
- 类型安全，利用 Rust 泛型

**实现**：

```rust
// 1. 原始数据缓存
pub struct RawDataCache {
    trades: Arc<RwLock<HashMap<DataKey, Arc<Vec<Trade>>>>>,
    klines: Arc<RwLock<HashMap<DataKey, Arc<Vec<Kline>>>>>,
}

// 2. 聚合数据缓存（泛型）
pub struct AggregatedDataCache<D: DataPoint> {
    cache: Arc<RwLock<HashMap<DataKey, Arc<TimeSeries<D>>>>>,
}

// 3. 统一数据管理器
pub struct UnifiedDataManager {
    raw_data: RawDataCache,
    kline_data: AggregatedDataCache<KlineDataPoint>,
    heatmap_data: AggregatedDataCache<HeatmapDataPoint>,
}
```

### 方案 B: 适配器模式

**核心思想**：
- 为不同图表类型提供适配器
- 统一的数据接口
- 内部处理类型转换

**实现**：

```rust
pub trait ChartDataAdapter {
    type Output;
    fn adapt(&self, raw_data: &RawData) -> Self::Output;
}

// Footprint/Candles 适配器
impl ChartDataAdapter for KlineChartAdapter {
    type Output = TimeSeries<KlineDataPoint>;
    fn adapt(&self, raw_data: &RawData) -> Self::Output {
        // 从原始数据聚合为 KlineDataPoint
    }
}

// Heatmap 适配器
impl ChartDataAdapter for HeatmapChartAdapter {
    type Output = TimeSeries<HeatmapDataPoint>;
    fn adapt(&self, raw_data: &RawData) -> Self::Output {
        // 从原始数据聚合为 HeatmapDataPoint
    }
}
```

### 方案 C: 混合方案（最佳）

结合方案 A 和 B：
- 原始数据层共享（方案 A）
- 聚合数据层按类型缓存（方案 A）
- 使用适配器处理类型转换（方案 B）

## 具体实现

### 1. 数据键设计

```rust
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct DataKey {
    pub ticker: TickerInfo,
    pub range: FetchRange,
    pub basis: Basis,  // 时间间隔或 Tick 数量
}

// 对于聚合数据，还需要聚合类型
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct AggregatedDataKey {
    pub base: DataKey,
    pub aggregation_type: AggregationType,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum AggregationType {
    KlineDataPoint,    // Footprint 和 Candles
    HeatmapDataPoint,  // Heatmap
}
```

### 2. 统一数据管理器

```rust
pub struct UnifiedDataManager {
    // 原始数据缓存
    raw_trades: Arc<RwLock<HashMap<DataKey, Arc<Vec<Trade>>>>>,
    raw_klines: Arc<RwLock<HashMap<DataKey, Arc<Vec<Kline>>>>>,
    
    // 聚合数据缓存
    kline_aggregated: Arc<RwLock<HashMap<AggregatedDataKey, Arc<TimeSeries<KlineDataPoint>>>>>,
    heatmap_aggregated: Arc<RwLock<HashMap<AggregatedDataKey, Arc<TimeSeries<HeatmapDataPoint>>>>>,
    
    // 请求去重
    pending_requests: Arc<RwLock<HashMap<DataKey, Vec<SubscriberId>>>>,
}

impl UnifiedDataManager {
    /// 请求原始交易数据
    pub fn request_trades(&self, key: DataKey, subscriber: SubscriberId) -> RequestResult<Arc<Vec<Trade>>> {
        // 检查缓存
        if let Some(cached) = self.raw_trades.read().unwrap().get(&key) {
            return RequestResult::Cached(Arc::clone(cached));
        }
        
        // 检查是否有进行中的请求
        // ... 去重逻辑
        
        RequestResult::NewRequest(key)
    }
    
    /// 请求 KlineDataPoint 聚合数据（Footprint/Candles）
    pub fn request_kline_data(
        &self,
        key: AggregatedDataKey,
        subscriber: SubscriberId,
    ) -> RequestResult<Arc<TimeSeries<KlineDataPoint>>> {
        // 1. 检查聚合数据缓存
        if let Some(cached) = self.kline_aggregated.read().unwrap().get(&key) {
            return RequestResult::Cached(Arc::clone(cached));
        }
        
        // 2. 检查原始数据是否可用
        let raw_key = key.base.clone();
        if let Some(trades) = self.raw_trades.read().unwrap().get(&raw_key) {
            // 从原始数据聚合
            let aggregated = self.aggregate_kline_data(trades, &key);
            // 缓存并返回
            return RequestResult::Cached(aggregated);
        }
        
        // 3. 需要先获取原始数据
        RequestResult::RequiresRawData(raw_key)
    }
    
    /// 请求 HeatmapDataPoint 聚合数据
    pub fn request_heatmap_data(
        &self,
        key: AggregatedDataKey,
        subscriber: SubscriberId,
    ) -> RequestResult<Arc<TimeSeries<HeatmapDataPoint>>> {
        // 类似逻辑，但使用不同的聚合方法
    }
}
```

### 3. 图表集成

```rust
// Footprint 和 Candles 使用相同的接口
impl KlineChart {
    pub fn request_data(&mut self, range: FetchRange) {
        let key = AggregatedDataKey {
            base: DataKey {
                ticker: self.chart.ticker_info,
                range,
                basis: self.chart.basis,
            },
            aggregation_type: AggregationType::KlineDataPoint,
        };
        
        match self.data_manager.request_kline_data(key, self.id) {
            RequestResult::Cached(data) => {
                self.data_source = PlotData::TimeBased((*data).clone());
                self.invalidate();
            }
            // ...
        }
    }
}

// Heatmap 使用不同的接口
impl HeatmapChart {
    pub fn request_data(&mut self, range: FetchRange) {
        let key = AggregatedDataKey {
            base: DataKey {
                ticker: self.chart.ticker_info,
                range,
                basis: self.chart.basis,
            },
            aggregation_type: AggregationType::HeatmapDataPoint,
        };
        
        match self.data_manager.request_heatmap_data(key, self.id) {
            RequestResult::Cached(data) => {
                self.trades = (*data).clone();
                self.invalidate();
            }
            // ...
        }
    }
}
```

## 数据流示例

### 场景：Footprint 和 Candles 请求相同数据

```
1. Footprint 请求 KlineDataPoint(1000-2000)
   └─> UnifiedDataManager::request_kline_data()
       ├─> 检查聚合缓存 → 未命中
       ├─> 检查原始数据 → 未命中
       └─> 发起原始数据请求

2. 原始数据到达
   └─> 缓存到 raw_trades
   └─> 自动聚合为 KlineDataPoint
   └─> 缓存到 kline_aggregated
   └─> 通知 Footprint

3. Candles 请求相同数据
   └─> UnifiedDataManager::request_kline_data()
       ├─> 检查聚合缓存 → 命中！✅
       └─> 直接返回 Arc<TimeSeries<KlineDataPoint>>
           └─> 零拷贝共享
```

### 场景：Heatmap 请求相同时间段的交易数据

```
1. Heatmap 请求 HeatmapDataPoint(1000-2000)
   └─> UnifiedDataManager::request_heatmap_data()
       ├─> 检查聚合缓存 → 未命中
       ├─> 检查原始数据 → 命中！✅（Footprint 已下载）
       └─> 从原始数据聚合为 HeatmapDataPoint
           └─> 缓存并返回
```

## 优势总结

| 特性 | Footprint/Candles | Heatmap | 说明 |
|------|------------------|---------|------|
| **原始数据共享** | ✅ | ✅ | 都使用相同的 Trade 数据 |
| **聚合数据共享** | ✅ | ❌ | Footprint/Candles 共享，Heatmap 独立 |
| **内存效率** | 高 | 中 | 原始数据共享，聚合数据按需 |
| **类型安全** | ✅ | ✅ | Rust 泛型保证 |

## 总结

**答案：不是完全相同的机制，而是分层共享**

1. **Footprint 和 Candles**：完全共享同一份 `TimeSeries<KlineDataPoint>`
2. **Heatmap**：共享原始 `Trade` 数据，但使用独立的 `TimeSeries<HeatmapDataPoint>` 聚合
3. **原始数据层**：所有图表共享 `Trade` 和 `Kline` 原始数据

这种设计既保证了内存效率，又保持了类型安全和各图表类型的独立性。

