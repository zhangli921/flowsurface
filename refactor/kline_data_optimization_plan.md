# K线数据获取优化方案

## 一、问题分析

### 1.1 当前架构问题

**当前实现**：
- K线数据通过聚合交易数据（tick data）得到
- `RealtimeDataService::fetch_kline_blocking` 从 MmapStore 读取交易数据，然后聚合为K线
- `HistoricalDataService::fetch_kline` 从 Parquet 读取交易数据，然后聚合为K线

**问题**：
1. **性能问题**：需要下载大量交易数据才能显示K线，导致K线图显示慢
2. **数据量问题**：交易数据量远大于K线数据量（例如：1分钟K线 vs 数百笔交易）
3. **用户体验问题**：用户需要等待交易数据下载完成才能看到K线图

### 1.2 用户需求

1. **K线数据**：
   - 应该快速获取和显示（使用专门的K线API）
   - 实时和历史数据都使用K线接口
   - 数据量小，响应快

2. **筹码峰（Volume Profile）**：
   - 需要交易数据（tick data）来计算
   - 可以异步、渐进式加载
   - 不影响K线图的显示

## 二、优化方案

### 2.1 架构设计原则

1. **数据源分离**：
   - **K线数据**：使用专门的K线API（REST + WebSocket）
   - **交易数据**：专门用于筹码峰计算

2. **性能优化**：
   - K线数据优先：快速获取，立即显示
   - 交易数据异步：后台加载，渐进式显示

3. **数据一致性**：
   - K线数据从交易所直接获取，保证准确性
   - 交易数据用于VP计算，不影响K线显示

### 2.2 新架构设计

```
┌─────────────────────────────────────────────────────────────┐
│                      UI Layer (Rendering)                    │
│  - K线图：快速显示（使用K线数据）                            │
│  - 筹码峰：渐进式显示（使用交易数据）                        │
└─────────────────────────────────────────────────────────────┘
                            │
        ┌───────────────────┴───────────────────┐
        │                                       │
        ▼                                       ▼
┌──────────────────┐                  ┌──────────────────┐
│  K线数据服务      │                  │  交易数据服务     │
│  (快速响应)       │                  │  (异步加载)       │
│                  │                  │                  │
│  - 实时K线       │                  │  - 实时交易      │
│  - 历史K线       │                  │  - 历史交易      │
└──────────────────┘                  └──────────────────┘
        │                                       │
        ▼                                       ▼
┌──────────────────┐                  ┌──────────────────┐
│  UnifiedDataService│                 │  UnifiedDataService│
│  (K线专用)        │                  │  (交易数据专用)   │
└──────────────────┘                  └──────────────────┘
        │                                       │
        ▼                                       ▼
┌──────────────────┐                  ┌──────────────────┐
│  RealtimeKline   │                  │  RealtimeTick    │
│  Service         │                  │  Service         │
│  - WebSocket     │                  │  - WebSocket     │
│  - REST API      │                  │  - REST API      │
└──────────────────┘                  └──────────────────┘
        │                                       │
        ▼                                       ▼
┌──────────────────┐                  ┌──────────────────┐
│  HistoricalKline │                  │  HistoricalTick  │
│  Service         │                  │  Service         │
│  - Binance Data  │                  │  - Binance Data  │
│    Vision        │                  │    Vision        │
└──────────────────┘                  └──────────────────┘
```

### 2.3 数据获取流程

#### 2.3.1 K线数据获取（快速路径）

**实时K线数据**：
1. **WebSocket流**（推荐）：
   - 订阅 Binance K线流：`ws://stream.binance.com:9443/ws/btcusdt@kline_1m`
   - 实时接收K线更新
   - 缓存到内存或轻量级存储（可选）

2. **REST API**（备用）：
   - 使用 `/api/v3/klines` 获取最近的K线数据
   - 用于初始加载或WebSocket断开时的补充

**历史K线数据**：
1. **Binance Data Vision**（推荐）：
   - 下载每日K线数据：`https://data.binance.vision/data/spot/daily/klines/BTCUSDT/1m/BTCUSDT-1m-2025-11-24.zip`
   - 缓存到本地 Parquet 文件
   - 按日期和周期组织

2. **REST API**（备用）：
   - 使用 `/api/v3/klines` 获取历史K线数据
   - 用于小范围查询或缓存未命中时

#### 2.3.2 交易数据获取（异步路径）

**实时交易数据**：
- 继续使用现有的 `RealtimeIngesterService`
- WebSocket流：`ws://stream.binance.com:9443/ws/btcusdt@aggTrade`
- 写入 MmapStore（用于VP计算）

**历史交易数据**：
- 继续使用现有的 `HistoricalIngesterService`
- 从 Binance Data Vision 下载：`https://data.binance.vision/data/spot/daily/aggTrades/BTCUSDT/BTCUSDT-aggTrades-2025-11-24.zip`
- 缓存到本地 Parquet 文件（用于VP计算）

### 2.4 组件重构

#### 2.4.1 新增组件

1. **`RealtimeKlineService`**：
   - 负责实时K线数据的获取
   - WebSocket订阅K线流
   - REST API获取初始数据
   - 可选：轻量级缓存（内存或Redis）

2. **`HistoricalKlineService`**：
   - 负责历史K线数据的获取
   - 从 Binance Data Vision 下载
   - 本地缓存管理
   - REST API备用

3. **`UnifiedKlineService`**：
   - 统一K线数据访问接口
   - 自动选择实时或历史数据源
   - 封装数据源选择逻辑

#### 2.4.2 修改现有组件

1. **`UnifiedDataService`**：
   - 分离K线数据获取和交易数据获取
   - `fetch_klines` 使用 `UnifiedKlineService`
   - `fetch_ticks` 继续使用现有的交易数据服务

2. **`RealtimeDataService`**：
   - 移除 `fetch_kline_blocking` 方法（不再从交易数据聚合）
   - 保留 `fetch_ticks_blocking` 方法（用于VP计算）

3. **`HistoricalDataService`**：
   - 移除 `fetch_kline` 方法（不再从交易数据聚合）
   - 保留 `fetch_ticks` 方法（用于VP计算）

4. **`RealtimeIngesterService`**：
   - 继续负责实时交易数据的下载
   - 可选：添加K线WebSocket订阅（用于实时K线更新）

5. **`HistoricalIngesterService`**：
   - 继续负责历史交易数据的下载
   - 新增：历史K线数据的下载和缓存

### 2.5 数据存储

#### 2.5.1 K线数据存储

**实时K线数据**：
- **方案A**（推荐）：内存缓存 + 可选持久化
  - 使用 `Arc<Vec<KLine>>` 或 `Arc<BTreeMap<u64, KLine>>` 存储
  - 可选：定期持久化到轻量级存储（SQLite或Parquet）

- **方案B**：直接使用REST API，不缓存
  - 每次请求都从API获取
  - 简单但可能有延迟

**历史K线数据**：
- 使用 Parquet 文件缓存
- 目录结构：`cache/{SYMBOL}/klines/{TIMEFRAME}/{DATE}.parquet`
- 例如：`cache/BTCUSDT/klines/1m/2025-11-24.parquet`

#### 2.5.2 交易数据存储

- 继续使用现有的 MmapStore（实时）和 Parquet（历史）
- 专门用于VP计算

### 2.6 性能优化

1. **K线数据优先加载**：
   - UI请求K线数据时，立即从K线API获取
   - 不等待交易数据下载完成

2. **交易数据异步加载**：
   - VP计算请求时，后台加载交易数据
   - 渐进式显示VP（部分数据也可以计算和显示）

3. **缓存策略**：
   - K线数据：内存缓存 + 本地Parquet缓存
   - 交易数据：MmapStore（实时）+ Parquet（历史）

4. **并发优化**：
   - K线数据获取和交易数据获取可以并行
   - 不影响彼此的性能

## 三、实现计划

### 3.1 阶段1：K线数据服务实现

1. **创建 `RealtimeKlineService`**：
   - 实现 WebSocket K线流订阅
   - 实现 REST API K线获取
   - 实现内存缓存

2. **创建 `HistoricalKlineService`**：
   - 实现 Binance Data Vision K线数据下载
   - 实现本地Parquet缓存
   - 实现REST API备用

3. **创建 `UnifiedKlineService`**：
   - 实现统一接口
   - 实现数据源选择逻辑
   - 实现时间边界处理

### 3.2 阶段2：集成到现有架构

1. **修改 `UnifiedDataService`**：
   - 添加 `UnifiedKlineService` 依赖
   - 修改 `fetch_klines` 使用 `UnifiedKlineService`
   - 保持 `fetch_ticks` 不变

2. **修改 `RealtimeDataService` 和 `HistoricalDataService`**：
   - 移除K线聚合逻辑
   - 保留交易数据获取逻辑

3. **修改 `RealtimeIngesterService` 和 `HistoricalIngesterService`**：
   - 可选：添加K线数据下载功能
   - 保持交易数据下载功能

### 3.3 阶段3：UI层优化

1. **K线图显示**：
   - 优先显示K线数据
   - 不等待交易数据

2. **筹码峰显示**：
   - 异步加载交易数据
   - 渐进式显示VP

## 四、技术细节

### 4.1 Binance K线API

**REST API**：
```
GET /api/v3/klines?symbol=BTCUSDT&interval=1m&startTime=...&endTime=...&limit=1000
```

**WebSocket流**：
```
wss://stream.binance.com:9443/ws/btcusdt@kline_1m
```

**Binance Data Vision**：
```
https://data.binance.vision/data/spot/daily/klines/BTCUSDT/1m/BTCUSDT-1m-2025-11-24.zip
```

### 4.2 数据格式转换

**Binance K线格式** → **内部KLine格式**：
- 时间戳：毫秒 → 微秒（`* 1_000`）
- 价格：字符串 → `f64`
- 成交量：字符串 → `f64`
- 交易笔数：字符串 → `u32`

### 4.3 错误处理

1. **WebSocket断开**：
   - 自动重连
   - 使用REST API补充数据

2. **API限流**：
   - 实现请求限流
   - 使用备用数据源

3. **缓存失效**：
   - 检测缓存文件损坏
   - 自动重新下载

## 五、优势分析

1. **性能提升**：
   - K线数据获取速度提升10-100倍（取决于交易数据量）
   - 用户体验显著改善

2. **架构清晰**：
   - K线数据和交易数据职责分离
   - 代码更易维护

3. **扩展性好**：
   - 可以独立优化K线数据获取
   - 可以独立优化交易数据获取

4. **资源优化**：
   - K线数据量小，存储和传输成本低
   - 交易数据只在需要时加载

## 六、风险评估

1. **数据一致性**：
   - K线数据和交易数据可能略有差异（交易所聚合方式不同）
   - **缓解**：K线数据优先，交易数据用于VP计算

2. **API依赖**：
   - 依赖Binance API可用性
   - **缓解**：实现备用数据源和错误处理

3. **实现复杂度**：
   - 需要新增多个服务组件
   - **缓解**：分阶段实现，逐步迁移

## 七、总结

本方案通过分离K线数据和交易数据的获取路径，实现了：
- **K线数据快速获取**：使用专门的K线API，快速响应
- **交易数据异步加载**：专门用于VP计算，不影响K线显示
- **架构清晰**：职责分离，易于维护和扩展

建议按照阶段1 → 阶段2 → 阶段3的顺序实现，确保每个阶段都可以独立测试和验证。

