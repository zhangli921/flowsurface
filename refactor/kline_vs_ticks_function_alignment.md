# K线 vs Ticks 函数对齐检查

## 函数签名对比

### RealtimeDataService

| 函数 | 签名 | 参数 | 返回值 |
|------|------|------|--------|
| **K线** | `fetch_kline_blocking` | `symbol: &str, range: TimeRange, timeframe: &str` | `Result<Vec<KLine>, DataError>` |
| **Ticks** | `fetch_ticks_blocking` | `symbol: &str, range: TimeRange` | `Result<TickDataBuffer, DataError>` |

**差异**：
- ❌ 函数名不一致：`fetch_kline_blocking`（单数）vs `fetch_ticks_blocking`（复数）
- ✅ 参数对齐：K线有`timeframe`参数（合理，因为K线需要指定周期）
- ✅ 返回值类型不同（合理，因为数据类型不同）

### HistoricalDataService

| 函数 | 签名 | 参数 | 返回值 |
|------|------|------|--------|
| **K线** | `fetch_kline` | `symbol: &str, range: TimeRange, timeframe: &str` | `Result<Vec<KLine>, DataError>` |
| **Ticks** | `fetch_ticks` | `symbol: &str, range: TimeRange` | `Result<TickDataBuffer, DataError>` |

**差异**：
- ❌ 函数名不一致：`fetch_kline`（单数）vs `fetch_ticks`（复数）
- ✅ 参数对齐：K线有`timeframe`参数（合理）
- ✅ 返回值类型不同（合理）

### UnifiedDataService

| 函数 | 签名 | 参数 | 返回值 |
|------|------|------|--------|
| **K线** | `fetch_klines` | `symbol: String, range: TimeRange, timeframe: &str` | `Result<Vec<KLine>, DataError>` |
| **Ticks** | `fetch_ticks` | `symbol: String, range: TimeRange` | `Result<TickDataBuffer, DataError>` |

**差异**：
- ✅ 函数名一致：都是复数形式
- ✅ 参数对齐：K线有`timeframe`参数（合理）
- ✅ 返回值类型不同（合理）

## 实现机制对比

### 实时数据（Speed Layer）

| 方面 | K线 | Ticks |
|------|-----|-------|
| **数据获取** | REST API（定期更新） | WebSocket流 |
| **缓存机制** | 内存缓存（`KlineCache`） | MmapStore（持久化） |
| **更新频率** | 每分钟 | 实时（WebSocket） |
| **缓存位置** | `RealtimeIngesterService`后台任务 | `RealtimeIngesterService`主任务 |
| **读取方式** | 优先从缓存，未命中从API | 从MmapStore读取 |

**差异**：
- ❌ 缓存机制不同：K线使用内存缓存，Ticks使用MmapStore
- ❌ 数据获取方式不同：K线使用REST API，Ticks使用WebSocket
- ❌ 更新频率不同：K线每分钟更新，Ticks实时更新

### 历史数据（Batch Layer）

| 方面 | K线 | Ticks |
|------|-----|-------|
| **数据获取** | Binance Data Vision | Binance Data Vision |
| **缓存机制** | Parquet文件 | Parquet文件 |
| **缓存位置** | `cache/{SYMBOL}/klines/{TIMEFRAME}/{DATE}.parquet` | `cache/{SYMBOL}/{DATE}_ticks.parquet` |
| **读取方式** | 先检查缓存，未命中触发下载 | 先检查缓存，未命中触发下载 |

**差异**：
- ✅ 实现机制一致：都使用Parquet缓存
- ✅ 读取方式一致：都先检查缓存，未命中触发下载

## 问题总结

### 1. 函数命名不一致

**问题**：
- `RealtimeDataService::fetch_kline_blocking`（单数）vs `fetch_ticks_blocking`（复数）
- `HistoricalDataService::fetch_kline`（单数）vs `fetch_ticks`（复数）
- `UnifiedDataService::fetch_klines`（复数）vs `fetch_ticks`（复数）✅

**建议**：
- 统一使用复数形式：`fetch_klines_blocking` 和 `fetch_klines`
- 或者统一使用单数形式：`fetch_kline_blocking` 和 `fetch_kline`（但返回值是`Vec`，所以复数更合理）

### 2. 实时数据缓存机制不一致

**问题**：
- K线使用内存缓存（`KlineCache`）
- Ticks使用MmapStore（持久化）

**分析**：
- K线数据量小，内存缓存足够
- Ticks数据量大，需要持久化存储
- 这个差异是合理的，因为数据特性不同

**建议**：
- 保持现状（数据特性不同，缓存方式不同是合理的）
- 或者：K线也可以使用MmapStore，但可能过度设计

### 3. 实时数据获取方式不一致

**问题**：
- K线使用REST API定期更新（每分钟）
- Ticks使用WebSocket实时更新

**分析**：
- K线数据更新频率低（按周期），REST API足够
- Ticks数据更新频率高（每笔交易），需要WebSocket
- 这个差异是合理的

**建议**：
- 保持现状（数据特性不同，获取方式不同是合理的）
- 或者：K线也可以使用WebSocket（`@kline_1m`流），但需要额外实现

## 对齐建议

### 必须对齐（函数命名）

1. **重命名函数**：
   - `RealtimeDataService::fetch_kline_blocking` → `fetch_klines_blocking`
   - `HistoricalDataService::fetch_kline` → `fetch_klines`

### 可选对齐（实现机制）

1. **实时数据缓存**：
   - 保持现状（数据特性不同，缓存方式不同是合理的）

2. **实时数据获取**：
   - 保持现状（数据特性不同，获取方式不同是合理的）
   - 或者：K线也可以使用WebSocket（需要额外实现）

## 结论

**当前状态**：
- ✅ 历史数据实现机制一致
- ❌ 函数命名不一致（单数vs复数）
- ⚠️ 实时数据实现机制不同（但这是合理的，因为数据特性不同）

**建议**：
1. **必须修复**：统一函数命名为复数形式
2. **可选优化**：保持实时数据实现机制的差异（因为数据特性不同）

