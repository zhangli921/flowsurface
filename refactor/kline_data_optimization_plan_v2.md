# K线数据获取优化方案 V2（优雅版本）

## 一、问题分析

### 1.1 当前问题

**当前实现**：
- K线数据通过聚合交易数据（tick data）得到
- `RealtimeDataService::fetch_kline_blocking` 从 MmapStore 读取交易数据，然后聚合为K线
- `HistoricalDataService::fetch_kline` 从 Parquet 读取交易数据，然后聚合为K线

**问题**：
1. **性能问题**：需要下载大量交易数据才能显示K线，导致K线图显示慢
2. **数据量问题**：交易数据量远大于K线数据量
3. **用户体验问题**：用户需要等待交易数据下载完成才能看到K线图

### 1.2 用户需求

1. **K线数据**：应该快速获取和显示（使用专门的K线API）
2. **交易数据**：专门用于筹码峰计算，可以异步加载

## 二、架构原则回顾

### 2.1 Lambda架构核心原则

1. **Speed Layer 和 Batch Layer 分离**：
   - Speed Layer：实时数据（Mmap）
   - Batch Layer：历史数据（Parquet/API）

2. **数据源选择在 UnifiedDataService**：
   - 渲染层不需要知道数据来源
   - 统一接口封装数据源选择逻辑

3. **架构对称性**：
   - `RealtimeDataService` ↔ `HistoricalDataService`
   - 两者都支持K线数据和交易数据
   - 通过 `UnifiedDataService` 统一访问

### 2.2 当前架构（符合Lambda架构）

```
UnifiedDataService
├── RealtimeDataService (Speed Layer)
│   ├── fetch_kline_blocking()  ← 当前：从交易数据聚合
│   └── fetch_ticks_blocking()  ← 用于VP计算
└── HistoricalDataService (Batch Layer)
    ├── fetch_kline()           ← 当前：从交易数据聚合
    └── fetch_ticks()           ← 用于VP计算
```

## 三、优化方案（优雅版本）

### 3.1 核心思路

**不新增服务，只修改现有服务的实现**：
- `RealtimeDataService::fetch_kline_blocking` 改为从K线API获取（而不是从交易数据聚合）
- `HistoricalDataService::fetch_kline` 改为从K线API获取（而不是从交易数据聚合）
- `fetch_ticks` 方法保持不变，专门用于VP计算

### 3.2 优化后的架构

```
UnifiedDataService
├── RealtimeDataService (Speed Layer)
│   ├── fetch_kline_blocking()  ← 优化：从K线API获取（WebSocket + REST）
│   └── fetch_ticks_blocking()  ← 不变：从Mmap读取交易数据（用于VP）
└── HistoricalDataService (Batch Layer)
    ├── fetch_kline()           ← 优化：从K线API获取（Binance Data Vision + REST）
    └── fetch_ticks()           ← 不变：从Parquet读取交易数据（用于VP）
```

### 3.3 关键优势

1. **保持架构对称性**：
   - 不新增服务，保持 `RealtimeDataService` ↔ `HistoricalDataService` 的对称性
   - 两者都支持K线数据和交易数据，职责清晰

2. **符合Lambda架构**：
   - Speed Layer 和 Batch Layer 的分离保持不变
   - 数据源选择逻辑仍在 `UnifiedDataService` 中

3. **最小化改动**：
   - 只修改现有方法的实现，不改变接口
   - `UnifiedDataService` 的调用方式不变

## 四、实现细节

### 4.1 RealtimeDataService 优化

**当前实现**：
```rust
pub fn fetch_kline_blocking(&self, symbol: &str, range: TimeRange) -> Result<Vec<KLine>, DataError> {
    // 从 MmapStore 读取交易数据
    let trades = self.fetch_ticks_blocking(symbol, range)?;
    // 聚合为K线
    aggregate_trades_to_klines(trades, timeframe)
}
```

**优化后实现**：
```rust
pub fn fetch_kline_blocking(&self, symbol: &str, range: TimeRange, timeframe: &str) -> Result<Vec<KLine>, DataError> {
    // 方案A：从K线WebSocket流获取（如果已订阅）
    if let Some(klines) = self.kline_cache.get(&(symbol, range)) {
        return Ok(klines);
    }
    
    // 方案B：从REST API获取
    let client = reqwest::blocking::Client::new();
    let url = format!(
        "https://api.binance.com/api/v3/klines?symbol={}&interval={}&startTime={}&endTime={}&limit=1000",
        symbol, timeframe, range.start_us / 1000, range.end_us / 1000
    );
    let response = client.get(&url).send()?;
    let klines: Vec<BinanceKline> = response.json()?;
    Ok(klines.into_iter().map(KLine::from).collect())
}
```

**关键点**：
- 保持方法签名基本不变（只添加 `timeframe` 参数）
- 不再依赖交易数据
- 可选：添加轻量级内存缓存（用于WebSocket流）

### 4.2 HistoricalDataService 优化

**当前实现**：
```rust
pub async fn fetch_kline(&self, symbol: &str, range: TimeRange, timeframe: &str) -> Result<Vec<KLine>, DataError> {
    // 从 Parquet 读取交易数据
    let ticks = self.fetch_ticks(symbol, range).await?;
    // 聚合为K线
    aggregate_ticks_to_klines(ticks, timeframe)
}
```

**优化后实现**：
```rust
pub async fn fetch_kline(&self, symbol: &str, range: TimeRange, timeframe: &str) -> Result<Vec<KLine>, DataError> {
    // 1. 检查本地缓存（Parquet）
    let dates = calculate_date_range(range);
    let mut all_klines = Vec::new();
    
    for date in dates {
        let cache_key = format!("{}_{}_{}.parquet", symbol, date, timeframe);
        let cache_path = self.cache_dir.join(cache_key);
        
        if cache_path.exists() {
            // 从缓存加载
            let klines = self.load_klines_from_cache(&cache_path).await?;
            all_klines.extend(klines);
        } else {
            // 2. 从 Binance Data Vision 下载
            let klines = self.ingester.download_and_cache_kline(symbol, &date, timeframe).await?;
            all_klines.extend(klines);
        }
    }
    
    // 3. 过滤到请求的时间范围
    all_klines.retain(|k| k.open_time_us >= range.start_us && k.open_time_us < range.end_us);
    
    Ok(all_klines)
}
```

**关键点**：
- 保持方法签名不变
- 优先使用本地缓存（Parquet）
- 缓存未命中时从 Binance Data Vision 下载
- 可选：REST API作为备用

### 4.3 UnifiedDataService 调整

**当前实现**：
```rust
pub async fn fetch_klines(&self, symbol: String, range: TimeRange, timeframe: &str) -> Result<Vec<KLine>, DataError> {
    let safe_cutoff = calculate_safe_historical_cutoff();
    
    if range.start_us >= safe_cutoff {
        // 使用 RealtimeDataService
        let result = tokio::task::spawn_blocking({
            let service = self.speed_layer.clone();
            move || service.fetch_kline_blocking(&symbol, range)
        }).await??;
        Ok(result)
    } else {
        // 使用 HistoricalDataService
        self.batch_layer.fetch_kline(&symbol, range, timeframe).await
    }
}
```

**优化后**：
- 接口保持不变
- 内部实现改为调用优化后的 `fetch_kline_blocking` 和 `fetch_kline`
- 添加 `timeframe` 参数传递

### 4.4 实时K线数据获取（可选优化）

**方案A：WebSocket流订阅**（推荐，但需要额外实现）
- 在 `RealtimeIngesterService` 中添加K线WebSocket订阅
- 实时更新到内存缓存
- `RealtimeDataService::fetch_kline_blocking` 优先从缓存读取

**方案B：REST API**（简单，当前实现）
- 每次请求都从REST API获取
- 简单但可能有延迟

**建议**：先实现方案B，后续可以优化为方案A。

## 五、数据存储

### 5.1 K线数据存储

**实时K线数据**：
- **方案A**（推荐）：内存缓存（`Arc<BTreeMap<u64, KLine>>`）
- **方案B**：每次从REST API获取（简单但慢）

**历史K线数据**：
- 使用 Parquet 文件缓存
- 目录结构：`cache/{SYMBOL}/klines/{TIMEFRAME}/{DATE}.parquet`
- 例如：`cache/BTCUSDT/klines/1m/2025-11-24.parquet`

### 5.2 交易数据存储（不变）

- 继续使用现有的 MmapStore（实时）和 Parquet（历史）
- 专门用于VP计算

## 六、实现计划

### 6.1 阶段1：修改 RealtimeDataService

1. 修改 `fetch_kline_blocking` 方法：
   - 添加 `timeframe` 参数
   - 改为从REST API获取K线数据
   - 移除从交易数据聚合的逻辑

2. 可选：添加内存缓存（用于WebSocket流）

### 6.2 阶段2：修改 HistoricalDataService

1. 修改 `fetch_kline` 方法：
   - 改为从 Binance Data Vision 下载K线数据
   - 实现本地Parquet缓存
   - 移除从交易数据聚合的逻辑

2. 在 `HistoricalIngesterService` 中添加K线数据下载功能（如果还没有）

### 6.3 阶段3：调整 UnifiedDataService

1. 修改 `fetch_klines` 方法：
   - 添加 `timeframe` 参数传递
   - 调用优化后的 `fetch_kline_blocking` 和 `fetch_kline`

### 6.4 阶段4：UI层调整

1. 修改 `Dashboard` 和 `KlineChart`：
   - 确保传递 `timeframe` 参数
   - K线图优先显示，不等待交易数据

## 七、优势分析

### 7.1 架构优势

1. **保持架构对称性**：
   - 不新增服务，保持 `RealtimeDataService` ↔ `HistoricalDataService` 的对称性
   - 符合Lambda架构的Speed Layer和Batch Layer分离

2. **最小化改动**：
   - 只修改现有方法的实现，不改变整体架构
   - `UnifiedDataService` 的接口基本不变

3. **职责清晰**：
   - `RealtimeDataService` 和 `HistoricalDataService` 都支持K线和交易数据
   - 通过方法名区分（`fetch_kline` vs `fetch_ticks`）

### 7.2 性能优势

1. **K线数据快速获取**：
   - 直接从K线API获取，不依赖交易数据
   - 数据量小，响应快

2. **交易数据异步加载**：
   - 专门用于VP计算
   - 不影响K线图显示

### 7.3 维护优势

1. **代码简洁**：
   - 不新增服务，减少代码复杂度
   - 保持现有架构的清晰性

2. **易于扩展**：
   - 后续可以添加WebSocket流订阅
   - 不影响现有代码结构

## 八、对比分析

### 8.1 V1方案（不优雅）

**问题**：
- 新增 `RealtimeKlineService`、`HistoricalKlineService`、`UnifiedKlineService`
- 与现有的 `RealtimeDataService`、`HistoricalDataService`、`UnifiedDataService` 重复
- 破坏了架构的对称性
- 增加了代码复杂度

### 8.2 V2方案（优雅）

**优势**：
- 不新增服务，只修改现有服务的实现
- 保持架构的对称性和清晰性
- 符合Lambda架构原则
- 最小化改动，易于维护

## 九、总结

V2方案通过**只修改现有服务的实现**，而不是新增服务，实现了：
- ✅ **保持架构对称性**：`RealtimeDataService` ↔ `HistoricalDataService`
- ✅ **符合Lambda架构**：Speed Layer 和 Batch Layer 分离
- ✅ **最小化改动**：只修改方法实现，不改变接口
- ✅ **职责清晰**：K线数据和交易数据通过方法名区分
- ✅ **性能优化**：K线数据快速获取，交易数据异步加载

这个方案更加优雅，更符合整体架构目标。

