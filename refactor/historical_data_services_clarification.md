# 历史数据服务组件澄清文档

本文档澄清历史数据相关的三个服务和它们的职责、函数区别。

## 一、三个服务的职责划分

### 1. `HistoricalDataService` (历史数据读取服务)
**文件**: `data/src/historical_data_service.rs`

**职责**: 
- **读取**已缓存的历史数据（从 Parquet 文件）
- 检查数据可用性索引
- 提交下载任务（但不执行下载）
- 为上层（UnifiedDataService）提供数据接口

**关键函数**:
- `fetch_klines()` - 获取 K 线数据
- `fetch_ticks()` - 获取 Tick 数据

**特点**: 
- 只读操作（从缓存读取）
- 不直接下载数据
- 通过 `HistoricalDownloadService` 提交下载任务

---

### 2. `HistoricalIngesterService` (历史数据下载器)
**文件**: `data/src/historical_ingester.rs`

**职责**:
- **下载**历史数据（从 Binance Data Vision）
- **写入**缓存文件（Parquet 格式）
- **读取**缓存文件（Parquet 格式）
- 管理下载锁（防止重复下载）

**关键函数**:
- `download_and_cache_kline()` - 下载并缓存 K 线数据
- `download_and_cache_ticks()` - 下载并缓存 Tick 数据
- `load_klines_from_cache()` - 从缓存加载 K 线数据
- `load_ticks_from_cache()` - 从缓存加载 Tick 数据
- `save_klines_to_cache()` - 保存 K 线数据到缓存
- `save_ticks_to_cache()` - 保存 Tick 数据到缓存

**特点**:
- 执行实际的下载操作
- 执行实际的缓存读写操作
- 被 `HistoricalDownloadService` 调用

---

### 3. `HistoricalDownloadService` (历史数据下载管理服务)
**文件**: `data/src/historical_download_service.rs`

**职责**:
- **管理**下载任务队列（优先级队列）
- **实现**重试机制（指数退避）
- **更新**数据可用性索引
- **发布**下载事件（通过 EventBus）
- **调度**下载任务（调用 `HistoricalIngesterService`）

**关键函数**:
- `submit_task()` - 提交下载任务到队列
- `run_download_loop()` - 运行下载循环（后台任务）
- `download_with_retry()` - 带重试的下载逻辑

**特点**:
- 不直接下载数据（委托给 `HistoricalIngesterService`）
- 管理任务队列和重试逻辑
- 发布事件通知其他组件

---

## 二、函数命名规律

### K-line vs Tick 数据

所有服务都区分两种数据类型：

| 函数名模式 | 说明 | 示例 |
|----------|------|------|
| `*_kline*` | 处理 K 线数据 | `fetch_klines()`, `download_and_cache_kline()` |
| `*_tick*` | 处理 Tick 数据 | `fetch_ticks()`, `download_and_cache_ticks()` |

**区别**:
- **K-line**: 聚合后的 OHLCV 数据（开高低收成交量），按时间间隔（1m, 5m, 1h 等）
- **Tick**: 原始交易数据（每笔交易的价格和数量），更细粒度

---

### 操作类型命名

| 函数前缀 | 说明 | 所在服务 |
|---------|------|---------|
| `fetch_*` | 获取数据（可能触发下载） | `HistoricalDataService` |
| `download_and_cache_*` | 下载并缓存数据 | `HistoricalIngesterService` |
| `load_*_from_cache` | 从缓存加载数据 | `HistoricalIngesterService` |
| `save_*_to_cache` | 保存数据到缓存 | `HistoricalIngesterService` |
| `submit_task` | 提交下载任务 | `HistoricalDownloadService` |

---

## 三、数据流向

```
用户请求数据
    ↓
UnifiedDataService
    ↓
HistoricalDataService.fetch_klines() / fetch_ticks()
    ├─→ 检查 DataAvailabilityIndex
    ├─→ 从缓存读取数据（如果有）
    └─→ 提交下载任务到 HistoricalDownloadService
            ↓
        HistoricalDownloadService.submit_task()
            ↓
        HistoricalDownloadService.run_download_loop()
            ↓
        HistoricalIngesterService.download_and_cache_*()
            ├─→ 从 Binance Data Vision 下载
            └─→ 保存到 Parquet 缓存文件
                ↓
        更新 DataAvailabilityIndex
        发布 DataEvent
```

---

## 四、常见混淆点

### 1. `HistoricalDataService` vs `HistoricalIngesterService`

**混淆**: 两个服务都有"历史数据"相关功能

**澄清**:
- `HistoricalDataService`: **读取**服务，面向用户请求
- `HistoricalIngesterService`: **下载**服务，面向数据源

**类比**:
- `HistoricalDataService` = 图书馆管理员（帮你找书）
- `HistoricalIngesterService` = 图书采购员（去出版社买书）

---

### 2. `download_and_cache_*` vs `fetch_*`

**混淆**: 都有"获取数据"的含义

**澄清**:
- `download_and_cache_*`: 实际执行下载和缓存操作（在 `HistoricalIngesterService`）
- `fetch_*`: 获取数据，优先从缓存读取，缺失时提交下载任务（在 `HistoricalDataService`）

**区别**:
```rust
// HistoricalDataService::fetch_klines()
// 1. 检查缓存
// 2. 如果有缓存，直接返回
// 3. 如果没有，提交下载任务（不等待）
// 4. 返回可用数据（可能不完整）

// HistoricalIngesterService::download_and_cache_kline()
// 1. 检查缓存
// 2. 如果有缓存，直接返回
// 3. 如果没有，执行下载（阻塞等待）
// 4. 保存到缓存
// 5. 返回数据
```

---

### 3. `HistoricalDownloadService` 的作用

**混淆**: 为什么需要这个服务？`HistoricalIngesterService` 不能直接下载吗？

**澄清**:
- `HistoricalIngesterService`: 执行单次下载操作
- `HistoricalDownloadService`: 管理下载任务队列、重试、优先级

**类比**:
- `HistoricalIngesterService` = 单个工人（执行任务）
- `HistoricalDownloadService` = 项目经理（分配任务、管理进度、处理失败）

---

## 五、使用示例

### 场景 1: 用户请求历史 K 线数据

```rust
// 1. UnifiedDataService 调用
let klines = historical_data_service.fetch_klines("BTCUSDT", range, "1m").await?;

// 2. HistoricalDataService 内部流程:
//    - 检查 DataAvailabilityIndex
//    - 从缓存读取可用数据
//    - 提交缺失数据的下载任务
//    - 返回可用数据（不等待下载完成）

// 3. HistoricalDownloadService 后台处理:
//    - 从队列取出任务
//    - 调用 HistoricalIngesterService.download_and_cache_kline()
//    - 重试失败的任务
//    - 更新 DataAvailabilityIndex
//    - 发布 DataEvent
```

### 场景 2: 手动触发下载

```rust
// 通过 HistoricalDownloadService 提交任务
download_service.submit_task(DownloadTask::new(
    "BTCUSDT".to_string(),
    "2025-11-25".to_string(),
    DataType::Kline { timeframe: Some("1m".to_string()) },
    Some("1m".to_string()),
    DownloadPriority::UserRequest,
)).await;

// HistoricalDownloadService 会:
// 1. 检查是否已在下载中
// 2. 添加到优先级队列
// 3. 后台循环处理任务
// 4. 调用 HistoricalIngesterService 执行下载
```

---

## 六、总结

| 服务 | 主要职责 | 调用者 | 被调用者 |
|------|---------|--------|---------|
| `HistoricalDataService` | 读取缓存数据，提交下载任务 | `UnifiedDataService` | `HistoricalDownloadService`, `DataAvailabilityIndex` |
| `HistoricalDownloadService` | 管理下载队列，重试逻辑 | `HistoricalDataService`, UI | `HistoricalIngesterService`, `DataAvailabilityIndex`, `EventBus` |
| `HistoricalIngesterService` | 执行下载和缓存操作 | `HistoricalDownloadService` | Binance Data Vision API |

**设计原则**:
- **单一职责**: 每个服务只负责一个明确的功能
- **分层架构**: 读取层 → 管理层 → 执行层
- **解耦**: 通过 EventBus 和接口实现组件间通信

