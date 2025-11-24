# K线 vs Ticks 函数对齐总结

## 对齐后的函数签名

### RealtimeDataService

| 函数 | 签名 | 参数 | 返回值 |
|------|------|------|--------|
| **K线** | `fetch_klines_blocking` | `symbol: &str, range: TimeRange, timeframe: &str` | `Result<Vec<KLine>, DataError>` |
| **Ticks** | `fetch_ticks_blocking` | `symbol: &str, range: TimeRange` | `Result<TickDataBuffer, DataError>` |

✅ **已对齐**：函数名都是复数形式

### HistoricalDataService

| 函数 | 签名 | 参数 | 返回值 |
|------|------|------|--------|
| **K线** | `fetch_klines` | `symbol: &str, range: TimeRange, timeframe: &str` | `Result<Vec<KLine>, DataError>` |
| **Ticks** | `fetch_ticks` | `symbol: &str, range: TimeRange` | `Result<TickDataBuffer, DataError>` |

✅ **已对齐**：函数名都是复数形式

### UnifiedDataService

| 函数 | 签名 | 参数 | 返回值 |
|------|------|------|--------|
| **K线** | `fetch_klines` | `symbol: String, range: TimeRange, timeframe: &str` | `Result<Vec<KLine>, DataError>` |
| **Ticks** | `fetch_ticks` | `symbol: String, range: TimeRange` | `Result<TickDataBuffer, DataError>` |

✅ **已对齐**：函数名都是复数形式

## 实现机制对比

### 实时数据（Speed Layer）

| 方面 | K线 | Ticks | 对齐状态 |
|------|-----|-------|---------|
| **数据获取** | REST API（定期更新） | WebSocket流 | ⚠️ 不同（合理） |
| **缓存机制** | 内存缓存（`KlineCache`） | MmapStore（持久化） | ⚠️ 不同（合理） |
| **更新频率** | 每分钟 | 实时（WebSocket） | ⚠️ 不同（合理） |
| **缓存位置** | `RealtimeIngesterService`后台任务 | `RealtimeIngesterService`主任务 | ⚠️ 不同（合理） |
| **读取方式** | 优先从缓存，未命中从API | 从MmapStore读取 | ⚠️ 不同（合理） |

**说明**：实现机制不同是合理的，因为：
- K线数据量小，更新频率低（按周期），内存缓存足够
- Ticks数据量大，更新频率高（每笔交易），需要持久化存储

### 历史数据（Batch Layer）

| 方面 | K线 | Ticks | 对齐状态 |
|------|-----|-------|---------|
| **数据获取** | Binance Data Vision | Binance Data Vision | ✅ 一致 |
| **缓存机制** | Parquet文件 | Parquet文件 | ✅ 一致 |
| **缓存位置** | `cache/{SYMBOL}/klines/{TIMEFRAME}/{DATE}.parquet` | `cache/{SYMBOL}/{DATE}_ticks.parquet` | ✅ 一致 |
| **读取方式** | 先检查缓存，未命中触发下载 | 先检查缓存，未命中触发下载 | ✅ 一致 |

✅ **已对齐**：历史数据实现机制完全一致

## 函数调用流程对比

### K线数据流程

```
UI Layer
  ↓
UnifiedDataService::fetch_klines(symbol, range, timeframe)
  ↓
根据时间范围选择：
  - range >= safe_cutoff: RealtimeDataService::fetch_klines_blocking
    - 优先从KlineCache读取
    - 缓存未命中：从REST API获取并更新缓存
  - range < safe_cutoff: HistoricalDataService::fetch_klines
    - 先检查Parquet缓存
    - 缓存未命中：触发下载并缓存
```

### Ticks数据流程

```
UI Layer
  ↓
UnifiedDataService::fetch_ticks(symbol, range)
  ↓
根据时间范围选择：
  - range >= safe_cutoff: RealtimeDataService::fetch_ticks_blocking
    - 从MmapStore读取
  - range < safe_cutoff: HistoricalDataService::fetch_ticks
    - 先检查Parquet缓存
    - 缓存未命中：触发下载并缓存
```

## 对齐状态总结

### ✅ 已对齐

1. **函数命名**：所有函数都使用复数形式
   - `fetch_klines_blocking` / `fetch_ticks_blocking`
   - `fetch_klines` / `fetch_ticks`

2. **历史数据实现机制**：完全一致
   - 都使用Parquet缓存
   - 都先检查缓存，未命中触发下载

3. **函数参数结构**：一致
   - 都有`symbol`和`range`参数
   - K线有额外的`timeframe`参数（合理）

### ⚠️ 实现机制不同（但合理）

1. **实时数据缓存**：
   - K线：内存缓存（数据量小）
   - Ticks：MmapStore（数据量大，需要持久化）

2. **实时数据获取**：
   - K线：REST API定期更新（更新频率低）
   - Ticks：WebSocket实时更新（更新频率高）

3. **更新频率**：
   - K线：每分钟更新（按周期）
   - Ticks：实时更新（每笔交易）

## 结论

✅ **函数命名已对齐**：所有函数都使用复数形式

✅ **历史数据实现机制已对齐**：完全一致

⚠️ **实时数据实现机制不同**：但这是合理的，因为数据特性不同：
- K线数据量小，更新频率低 → 内存缓存 + REST API
- Ticks数据量大，更新频率高 → MmapStore + WebSocket

**总体评价**：函数命名和接口已对齐，实现机制的差异是合理的，符合数据特性。

