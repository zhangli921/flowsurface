# 历史数据完整性改进设计方案

## 文档说明

本文档描述了历史数据下载和完整性保证的系统性改进方案，旨在解决当前历史数据下载不完整、用户体验差的问题。

**目标读者**：架构师、程序员、测试人员

**文档目的**：提供清晰、完整、分阶段可执行的设计规范

**设计原则**：
- 保持 Lambda 架构的清晰性
- 异步非阻塞设计
- 事件驱动，组件解耦
- 单一职责原则
- 可扩展性

---

## 一、问题分析

### 1.1 当前问题

1. **被动下载模式**
   - 仅在 `fetch_ticks` 时按需下载
   - 下载失败（如 404）直接返回空数据，导致后续计算失败

2. **缺少重试机制**
   - 网络错误或临时失败没有自动重试
   - 404 错误被当作正常情况处理

3. **无后台预加载**
   - 没有主动预加载常用时间范围
   - 用户查看历史数据时需要等待下载

4. **数据完整性验证不足**
   - 下载后未验证是否覆盖请求的时间范围
   - 缓存文件损坏时可能返回不完整数据

5. **错误处理过于宽松**
   - 下载失败时静默返回空数据
   - 缺少错误报告和恢复机制

### 1.2 影响

- 筹码峰（VP）计算频繁失败
- 用户体验差（需要等待数据下载）
- 系统资源浪费（重复尝试下载不可用数据）

---

## 二、架构设计

### 2.1 整体架构

```
┌─────────────────────────────────────────────────────────────┐
│                    Application Layer                        │
│  (main.rs, Dashboard, Chart)                               │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
        ┌───────────────────────────────────────┐
        │   UnifiedDataService (统一接口)        │
        │  - 自动选择数据源                      │
        │  - 返回数据和元数据                    │
        └───────────────────────────────────────┘
                            │
        ┌───────────────────┴───────────────────┐
        │                                       │
        ▼                                       ▼
┌──────────────────────┐          ┌──────────────────────┐
│ RealtimeDataService  │          │ HistoricalDataService│
│ (实时数据读取)        │          │ (历史数据读取)        │
│                      │          │                      │
│ - 从 Mmap 读取       │          │ - 从缓存读取          │
│ - 聚合 K 线          │          │ - 检查数据可用性      │
│                      │          │ - 触发下载请求        │
└──────────────────────┘          └──────────────────────┘
        ▲                                       ▲
        │                                       │
        │                                       │
┌──────────────────────┐          ┌──────────────────────┐
│RealtimeIngesterService│          │HistoricalIngesterService│
│ (实时数据写入)        │          │ (历史数据下载)        │
│                      │          │                      │
│ - WebSocket 接收     │          │ - 下载任务队列        │
│ - 写入 Mmap          │          │ - 重试机制            │
│                      │          │ - 数据完整性验证      │
└──────────────────────┘          └──────────────────────┘
        │                                       │
        │                    ┌──────────────────┴──────────────┐
        │                    │                                  │
        ▼                    ▼                                  ▼
┌──────────────────────┐  ┌──────────────────────┐  ┌──────────────────────┐
│  market_data/        │  │  cache/              │  │  DataAvailabilityIndex│
│  {SYMBOL}.mmap       │  │  {DATE}-*.parquet    │  │  (数据可用性索引)     │
└──────────────────────┘  └──────────────────────┘  └──────────────────────┘
                                                              │
                                                              │
                    ┌─────────────────────────────────────────┘
                    │
                    ▼
        ┌──────────────────────┐
        │  PreloadService      │
        │  (智能预加载服务)     │
        │  [第二阶段]          │
        │                      │
        │  - 监听用户行为      │
        │  - 预测数据需求      │
        │  - 提交下载任务      │
        └──────────────────────┘
```

### 2.2 核心组件

#### 2.2.1 DataAvailabilityIndex（数据可用性索引）

**位置**：`flowsurface/data/src/data_availability_index.rs`

**职责**：
- 维护每个交易对每个日期的数据状态
- 避免重复尝试下载不可用数据
- 支持持久化（应用重启后保持状态）

**数据结构**：
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataAvailability {
    Available,      // 数据已缓存，可用
    Downloading,    // 正在下载中
    Unavailable,    // 数据不可用（404，或超出发布延迟）
    Partial,        // 部分数据（缓存损坏或不完整）
    Unknown,        // 未知状态（首次查询）
}

pub struct DataAvailabilityIndex {
    // symbol -> date -> (availability, last_checked)
    index: Arc<RwLock<HashMap<String, HashMap<String, (DataAvailability, Instant)>>>>,
    persistence_path: PathBuf,
}
```

**关键方法**：
- `check_availability(symbol, date) -> DataAvailability`
- `update_availability(symbol, date, status)`
- `check_date_range(symbol, dates) -> Vec<(date, availability)>`
- `load_from_disk()` / `save_to_disk()`

#### 2.2.2 HistoricalDownloadService（历史数据下载服务）

**位置**：`flowsurface/data/src/historical_download_service.rs`

**职责**：
- 管理下载任务队列（优先级队列）
- 实现重试机制（指数退避）
- 更新数据可用性索引
- 发送下载事件

**数据结构**：
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DownloadPriority {
    UserRequest = 0,    // 用户请求（最高优先级）
    Preload = 1,        // 预加载（中等优先级）
    Background = 2,     // 后台任务（最低优先级）
}

pub struct DownloadTask {
    symbol: String,
    date: String,
    data_type: DataType, // Kline or Tick
    timeframe: Option<String>, // For Kline
    priority: DownloadPriority,
    retry_count: u32,
    created_at: Instant,
}

pub struct HistoricalDownloadService {
    task_queue: Arc<Mutex<PriorityQueue<DownloadTask>>>,
    availability_index: Arc<DataAvailabilityIndex>,
    ingester: Arc<HistoricalIngesterService>,
    event_tx: tokio::sync::broadcast::Sender<DownloadEvent>,
    max_concurrent_downloads: usize,
    current_downloads: Arc<Mutex<HashSet<String>>>, // 正在下载的任务
}
```

**关键方法**：
- `new() -> (Self, Receiver<DownloadEvent>)`
- `submit_task(task: DownloadTask)`
- `run_download_loop()` // 后台任务循环
- `download_with_retry(task) -> Result<(), DataError>`

**重试策略**：
```rust
// 指数退避重试
// 临时错误（网络、超时）：立即重试，最多 3 次
// 永久错误（404）：延迟 1 小时后重试（数据可能尚未发布）
// 403 错误：标记为 Unavailable，不再重试
```

#### 2.2.3 修改 HistoricalDataService（历史数据读取服务）

**位置**：`flowsurface/data/src/historical_data_service.rs`

**修改点**：
1. 添加 `availability_index` 和 `download_service` 依赖
2. 在 `fetch_ticks` 中检查数据可用性
3. 提交下载任务（不阻塞）
4. 返回部分数据（即使不完整）

**关键修改**：
```rust
impl HistoricalDataService {
    pub async fn fetch_ticks(
        &self,
        symbol: &str,
        range: TimeRange,
    ) -> Result<TickDataBuffer, DataError> {
        // 1. 计算日期范围
        let dates = calculate_date_range(range);
        
        // 2. 检查数据可用性索引
        let missing_dates = self.availability_index
            .check_date_range(symbol, &dates)
            .await
            .into_iter()
            .filter(|(_, status)| {
                matches!(status, DataAvailability::Unknown | DataAvailability::Unavailable)
            })
            .map(|(date, _)| date)
            .collect::<Vec<_>>();
        
        // 3. 提交下载任务（UserRequest 优先级，不阻塞）
        for date in &missing_dates {
            self.download_service.submit_task(DownloadTask {
                symbol: symbol.to_string(),
                date: date.clone(),
                data_type: DataType::Tick,
                timeframe: None,
                priority: DownloadPriority::UserRequest,
                retry_count: 0,
                created_at: Instant::now(),
            }).await;
        }
        
        // 4. 加载可用数据（不等待下载完成）
        // 返回部分数据 + 完整性信息
        // ...
    }
}
```

#### 2.2.4 PreloadService（智能预加载服务）[第二阶段]

**位置**：`flowsurface/data/src/preload_service.rs`

**职责**：
- 监听用户查看范围变化
- 预测并预加载相邻范围
- 启动时预加载常用数据

**数据结构**：
```rust
pub struct PreloadService {
    download_service: Arc<HistoricalDownloadService>,
    availability_index: Arc<DataAvailabilityIndex>,
    current_view_ranges: Arc<RwLock<HashMap<String, TimeRange>>>,
    user_behavior: Arc<RwLock<UserBehaviorHistory>>,
    config: PreloadConfig,
}

pub struct PreloadConfig {
    preload_days_forward: u64,      // 向前预加载天数（动态计算）
    min_preload_interval: Duration,  // 最小预加载间隔（如 5 秒）
    max_concurrent_preloads: usize,  // 最大并发预加载任务数（如 3）
    enable_prediction: bool,         // 是否启用预测
}
```

**预加载逻辑**：
1. **触发时机**：
   - 用户查看范围变化时（显著变化）
   - 应用启动时（预加载常用范围）
   - 用户订阅新交易对时

2. **预加载范围计算**：
   ```rust
   // 根据当前查看范围的时间跨度动态调整预加载天数
   match range_span_days {
       0 => 3,        // < 1 天，预加载前后各 3 天
       1..=7 => 7,    // 1-7 天，预加载前后各 7 天
       8..=30 => 14,  // 7-30 天，预加载前后各 14 天
       _ => 30,       // > 30 天，预加载前后各 30 天
   }
   ```

3. **资源限制**：
   - 节流：最小预加载间隔（避免频繁触发）
   - 并发限制：最大并发预加载任务数
   - 优先级：预加载任务优先级低于用户请求

---

## 三、实施计划

### 阶段一：核心基础（立即实施）

**目标**：解决数据完整性问题的核心机制

#### 3.1.1 实现 DataAvailabilityIndex

**任务**：
1. 创建 `data_availability_index.rs`
2. 实现数据可用性状态管理
3. 实现持久化（JSON 文件）
4. 添加单元测试

**文件结构**：
```
flowsurface/data/src/
  ├── data_availability_index.rs (新建)
  └── mod.rs (添加模块导出)
```

**关键功能**：
- 检查数据可用性
- 更新数据状态
- 批量检查日期范围
- 持久化到磁盘

#### 3.1.2 实现 HistoricalDownloadService

**任务**：
1. 创建 `historical_download_service.rs`
2. 实现下载任务队列（优先级队列）
3. 实现重试机制（指数退避）
4. 实现事件系统
5. 集成到 HistoricalIngesterService

**文件结构**：
```
flowsurface/data/src/
  ├── historical_download_service.rs (新建)
  └── mod.rs (添加模块导出)
```

**关键功能**：
- 下载任务队列管理
- 重试机制（区分临时/永久错误）
- 更新数据可用性索引
- 发送下载事件

**重试策略**：
```rust
// 临时错误（网络、超时）
// - 立即重试，最多 3 次
// - 指数退避：1s, 2s, 4s

// 永久错误（404）
// - 延迟 1 小时后重试（数据可能尚未发布）
// - 最多重试 3 次

// 403 错误
// - 标记为 Unavailable，不再重试
```

#### 3.1.3 修改 HistoricalDataService

**任务**：
1. 添加 `availability_index` 和 `download_service` 依赖
2. 修改 `fetch_ticks` 和 `fetch_klines` 方法
3. 集成数据可用性检查
4. 集成下载任务提交

**修改点**：
- 构造函数添加新参数
- `fetch_ticks` 添加可用性检查和任务提交
- `fetch_klines` 添加可用性检查和任务提交
- 返回部分数据（即使不完整）

#### 3.1.4 修改 HistoricalIngesterService

**任务**：
1. 添加重试机制到 `download_and_cache_ticks`
2. 添加数据完整性验证
3. 更新数据可用性索引
4. 改进错误处理

**修改点**：
- `download_ticks_for_date` 添加重试逻辑
- 下载后验证数据完整性
- 更新可用性索引状态
- 区分临时错误和永久错误

#### 3.1.5 修改 main.rs 初始化

**任务**：
1. 创建 DataAvailabilityIndex
2. 创建 HistoricalDownloadService
3. 修改 HistoricalDataService 初始化
4. 订阅下载事件（可选，用于日志）

**代码示例**：
```rust
// 1. 创建数据可用性索引
let availability_index = Arc::new(DataAvailabilityIndex::new());

// 2. 创建历史数据下载器
let ingester = Arc::new(HistoricalIngesterService::new(None));

// 3. 创建下载服务（启动后台任务）
let (download_service, download_events) = HistoricalDownloadService::new(
    availability_index.clone(),
    ingester.clone(),
);

// 4. 创建历史数据读取服务
let historical_data_service = Arc::new(HistoricalDataService::new(
    ingester.clone(),
    availability_index.clone(),
    download_service.clone(),
));

// 5. 创建统一数据服务
let unified_data_service = Arc::new(UnifiedDataService::new(
    Arc::new(realtime_data_service),
    historical_data_service,
));
```

#### 3.1.6 实现历史数据状态窗口

**任务**：
1. 创建 `historical_data_status_window.rs`
2. 实现数据状态展示界面
3. 实现实时更新机制
4. 集成到主应用

**文件结构**：
```
flowsurface/src/
  ├── screen/
  │   └── historical_data_status/
  │       ├── mod.rs
  │       ├── window.rs (新建)
  │       └── table.rs (新建)
  └── main.rs (添加窗口管理)
```

**窗口功能**：
- 显示所有交易对的历史数据状态
- 按日期列表显示数据可用性
- 显示下载进度和状态
- 支持手动触发下载
- 支持筛选和搜索
- 实时更新状态

**UI 设计**：
```
┌─────────────────────────────────────────────────────────┐
│  历史数据状态                                    [×]     │
├─────────────────────────────────────────────────────────┤
│  [刷新] [下载选中] [筛选: BTCUSDT ▼] [搜索: ____]      │
├─────────────────────────────────────────────────────────┤
│  交易对    │ 日期       │ 状态      │ 大小    │ 操作   │
├─────────────────────────────────────────────────────────┤
│  BTCUSDT   │ 2025-11-25 │ ✅ 可用   │ 12.5 MB │ [重下] │
│  BTCUSDT   │ 2025-11-24 │ ⏳ 下载中 │ -       │ [取消] │
│  BTCUSDT   │ 2025-11-23 │ ❌ 不可用 │ -       │ [下载] │
│  BTCUSDT   │ 2025-11-22 │ ⚠️  部分  │ 8.2 MB  │ [修复] │
│  ETHUSDT   │ 2025-11-25 │ ✅ 可用   │ 10.3 MB │ [重下] │
│  ...       │ ...        │ ...       │ ...     │ ...    │
└─────────────────────────────────────────────────────────┘
```

**数据结构**：
```rust
pub struct HistoricalDataStatusWindow {
    availability_index: Arc<DataAvailabilityIndex>,
    download_service: Arc<HistoricalDownloadService>,
    // 当前显示的数据
    data_rows: Vec<DataStatusRow>,
    // 筛选条件
    filter_symbol: Option<String>,
    search_query: String,
    // 选中的行
    selected_rows: HashSet<(String, String)>, // (symbol, date)
    // 窗口ID
    window_id: window::Id,
}

pub struct DataStatusRow {
    symbol: String,
    date: String,
    status: DataAvailability,
    file_size: Option<u64>, // bytes
    last_checked: Option<Instant>,
    download_progress: Option<f32>, // 0.0 - 1.0
}
```

**关键方法**：
- `new() -> (Self, window::Id)`
- `view() -> Element<Message>`
- `update(message: Message) -> Task<Message>`
- `refresh_data() -> Task<Message>`
- `trigger_download(symbol, date) -> Task<Message>`

**实时更新机制**：
- 订阅下载事件（DownloadEvent）
- 定期刷新数据状态（每 5 秒）
- 下载进度实时更新

#### 3.1.7 测试和验证

**测试点**：
1. 数据可用性索引的持久化
2. 下载任务队列的优先级
3. 重试机制的正确性
4. 数据完整性验证
5. 部分数据返回
6. 历史数据状态窗口功能

**验收标准**：
- 数据可用性索引正常工作
- 下载任务按优先级执行
- 重试机制正确处理各种错误
- 数据完整性验证准确
- 部分数据可以正常返回
- 历史数据状态窗口正常显示和更新

### 阶段二：智能预加载 + 历史数据状态窗口（短期实施）

**目标**：提升用户体验，减少等待时间，提供数据状态可视化

#### 3.2.1 实现 PreloadService

**任务**：
1. 创建 `preload_service.rs`
2. 实现预加载逻辑
3. 实现用户行为跟踪
4. 实现预测逻辑（可选）

**文件结构**：
```
flowsurface/data/src/
  ├── preload_service.rs (新建)
  └── mod.rs (添加模块导出)
```

**关键功能**：
- 监听用户查看范围变化
- 计算预加载范围
- 提交预加载任务
- 资源限制和节流

#### 3.2.2 集成到 Chart 层

**任务**：
1. 在 `KlineChart::invalidate` 中调用预加载服务
2. 在 `check_data_update_needed` 中触发预加载
3. 传递查看范围到预加载服务

**修改点**：
- `flowsurface/src/chart/kline.rs`
- `flowsurface/src/screen/dashboard/pane.rs`

#### 3.2.3 启动时预加载

**任务**：
1. 在 `main.rs` 中启动预加载服务
2. 预加载常用交易对的常用范围
3. 预加载最近 7 天的数据

#### 3.2.4 完善历史数据状态窗口

**任务**：
1. 添加筛选和搜索功能
2. 添加批量操作（批量下载、批量删除）
3. 添加数据统计（总大小、可用率等）
4. 优化UI和交互

**功能增强**：
- 按状态筛选（可用/不可用/下载中/部分）
- 按交易对筛选
- 搜索功能（交易对、日期）
- 批量选择和多选操作
- 数据统计面板（总大小、可用率、缺失数量等）
- 导出数据列表（CSV）

**UI 增强**：
```
┌─────────────────────────────────────────────────────────┐
│  历史数据状态                                    [×]     │
├─────────────────────────────────────────────────────────┤
│  [刷新] [下载选中] [筛选: BTCUSDT ▼] [搜索: ____]      │
│  统计: 总计 1.2 GB | 可用率 85% | 缺失 15 个           │
├─────────────────────────────────────────────────────────┤
│  [☑] 交易对    │ 日期       │ 状态      │ 大小    │ 操作   │
├─────────────────────────────────────────────────────────┤
│  [☑] BTCUSDT   │ 2025-11-25 │ ✅ 可用   │ 12.5 MB │ [重下] │
│  [ ] BTCUSDT   │ 2025-11-24 │ ⏳ 下载中 │ -       │ [取消] │
│  [☑] BTCUSDT   │ 2025-11-23 │ ❌ 不可用 │ -       │ [下载] │
│  ...           │ ...        │ ...       │ ...     │ ...    │
└─────────────────────────────────────────────────────────┘
```

#### 3.2.5 测试和验证

**测试点**：
1. 预加载范围计算正确性
2. 预加载任务优先级
3. 资源限制和节流
4. 用户体验改善
5. 历史数据状态窗口功能完整性

**验收标准**：
- 预加载范围计算准确
- 预加载任务不影响用户请求
- 资源使用合理
- 用户等待时间明显减少
- 历史数据状态窗口功能完整且易用

### 阶段三：事件驱动和高级特性（长期优化）

**目标**：架构优化，支持高级功能

#### 3.3.1 完善事件系统

**任务**：
1. 定义完整的事件类型
2. 实现事件订阅机制
3. 实现事件广播

**事件类型**：
```rust
pub enum DownloadEvent {
    DownloadCompleted { symbol: String, date: String, data_type: DataType },
    DownloadFailed { symbol: String, date: String, error: DataError },
    AvailabilityChanged { symbol: String, date: String, status: DataAvailability },
}
```

#### 3.3.2 自动触发相关计算

**任务**：
1. VP 计算服务订阅下载事件
2. 数据下载完成后自动触发 VP 计算
3. 解耦数据层和计算层

#### 3.3.3 数据质量监控

**任务**：
1. 监控数据完整性
2. 报告数据缺失情况
3. UI 显示数据状态

#### 3.3.4 高级预测功能

**任务**：
1. 用户行为分析
2. 智能预测下一个查看范围
3. 自适应预加载策略

---

## 四、技术细节

### 4.1 数据可用性索引持久化

**格式**：JSON 文件
**位置**：`~/.local/share/flowsurface/data_availability_index.json`

**结构**：
```json
{
  "BTCUSDT": {
    "2025-11-20": {
      "status": "Available",
      "last_checked": "2025-11-25T10:00:00Z"
    },
    "2025-11-21": {
      "status": "Unavailable",
      "last_checked": "2025-11-25T10:00:00Z",
      "reason": "404"
    }
  }
}
```

### 4.2 下载任务队列

**实现**：使用 `std::collections::BinaryHeap` 实现优先级队列

**排序规则**：
1. 优先级（UserRequest < Preload < Background）
2. 创建时间（相同优先级按创建时间排序）

### 4.3 重试机制

**临时错误**（网络、超时）：
- 立即重试，最多 3 次
- 指数退避：1s, 2s, 4s

**永久错误**（404）：
- 延迟 1 小时后重试（数据可能尚未发布）
- 最多重试 3 次
- 如果 3 次都失败，标记为 Unavailable

**403 错误**：
- 标记为 Unavailable，不再重试

### 4.4 数据完整性验证

**验证点**：
1. 时间范围覆盖：下载的数据是否覆盖请求的时间范围
2. 数据量验证：一天的数据量是否合理（如 tick 数据约 86400 条）
3. 时间戳连续性：数据时间戳是否连续

### 4.5 预加载范围计算

**策略**：
```rust
fn calculate_preload_days(range_span_days: u64) -> u64 {
    match range_span_days {
        0 => 3,        // < 1 天，预加载前后各 3 天
        1..=7 => 7,    // 1-7 天，预加载前后各 7 天
        8..=30 => 14,  // 7-30 天，预加载前后各 14 天
        _ => 30,       // > 30 天，预加载前后各 30 天
    }
}
```

### 4.6 资源限制

**并发限制**：
- 最大并发下载任务数：5
- 最大并发预加载任务数：3

**节流**：
- 最小预加载间隔：5 秒
- 最小下载间隔：1 秒（同一日期）

---

## 五、错误处理

### 5.1 错误分类

**临时错误**：
- 网络超时
- 连接错误
- 服务器错误（5xx）

**永久错误**：
- 404 Not Found（数据不存在或尚未发布）
- 403 Forbidden（权限问题）

### 5.2 错误处理策略

**临时错误**：
- 立即重试（指数退避）
- 最多重试 3 次
- 如果全部失败，标记为 Unavailable，延迟 1 小时后重试

**永久错误**：
- 404：延迟 1 小时后重试（数据可能尚未发布）
- 403：标记为 Unavailable，不再重试

### 5.3 错误报告

**日志级别**：
- 临时错误：WARN
- 永久错误：INFO（404 是正常情况）
- 系统错误：ERROR

**用户通知**：
- 数据缺失时在 UI 显示提示
- 下载失败时记录但不阻塞用户操作

---

## 六、性能考虑

### 6.1 内存使用

**数据可用性索引**：
- 每个条目约 100 字节
- 1000 个交易对 × 365 天 ≈ 36.5 MB（可接受）

**下载任务队列**：
- 每个任务约 200 字节
- 最大队列长度：1000 个任务 ≈ 200 KB（可接受）

### 6.2 磁盘使用

**数据可用性索引持久化**：
- JSON 文件，压缩后约 1-5 MB（可接受）

### 6.3 CPU 使用

**后台任务**：
- 下载任务在独立线程池中执行
- 不影响主线程性能

**预加载服务**：
- 节流和并发限制避免 CPU 过载

---

## 七、测试策略

### 7.1 单元测试

**DataAvailabilityIndex**：
- 状态检查
- 状态更新
- 持久化

**HistoricalDownloadService**：
- 任务队列
- 重试机制
- 事件发送

**PreloadService**：
- 范围计算
- 预加载逻辑
- 资源限制

### 7.2 集成测试

**数据下载流程**：
- 正常下载
- 重试机制
- 错误处理

**预加载流程**：
- 范围变化触发
- 预加载任务提交
- 资源限制

### 7.3 性能测试

**并发下载**：
- 多个任务并发执行
- 资源使用情况

**预加载性能**：
- 预加载对系统性能的影响
- 用户体验改善

---

## 七、历史数据状态窗口详细设计

### 7.1 窗口架构

**位置**：`flowsurface/src/screen/historical_data_status/`

**文件结构**：
```
historical_data_status/
  ├── mod.rs              // 模块导出
  ├── window.rs           // 窗口主逻辑
  ├── table.rs            // 数据表格组件
  └── status_badge.rs     // 状态徽章组件
```

### 7.2 窗口功能

#### 7.2.1 数据展示

**表格列**：
1. **交易对**：显示交易对名称（如 BTCUSDT）
2. **日期**：显示日期（YYYY-MM-DD）
3. **状态**：显示数据可用性状态（带颜色和图标）
4. **大小**：显示文件大小（如果可用）
5. **最后检查**：显示最后检查时间
6. **操作**：操作按钮（下载/重试/删除等）

**状态显示**：
- ✅ **Available**（绿色）：数据已缓存，可用
- ⏳ **Downloading**（蓝色）：正在下载中，显示进度条
- ❌ **Unavailable**（红色）：数据不可用
- ⚠️ **Partial**（黄色）：部分数据，可能损坏
- ❓ **Unknown**（灰色）：未知状态

#### 7.2.2 交互功能

**筛选**：
- 按交易对筛选（下拉选择）
- 按状态筛选（多选）
- 按日期范围筛选

**搜索**：
- 搜索交易对名称
- 搜索日期

**操作**：
- 单个下载/重试
- 批量下载选中项
- 删除缓存文件
- 刷新状态

**排序**：
- 按交易对排序
- 按日期排序
- 按状态排序
- 按大小排序

#### 7.2.3 实时更新

**更新机制**：
1. **事件驱动**：订阅下载事件，实时更新状态
2. **定时刷新**：每 5 秒刷新一次数据状态
3. **手动刷新**：用户点击刷新按钮

**更新内容**：
- 下载进度实时更新
- 状态变化实时反映
- 文件大小实时更新

### 7.3 窗口集成

#### 7.3.1 主应用集成

**在 main.rs 中添加**：
```rust
struct Flowsurface {
    // ... 现有字段
    historical_data_status_window: Option<window::Id>,
    historical_data_status_state: Option<screen::historical_data_status::State>,
}

enum Message {
    // ... 现有消息
    OpenHistoricalDataStatusWindow,
    HistoricalDataStatus(screen::historical_data_status::Message),
    HistoricalDataStatusWindowClosed(window::Id),
}
```

**打开窗口**：
```rust
Message::OpenHistoricalDataStatusWindow => {
    let (state, window_id) = screen::historical_data_status::State::new(
        self.availability_index.clone(),
        self.download_service.clone(),
    );
    
    let window_config = window::Settings {
        size: Size::new(1000.0, 700.0),
        title: Some("历史数据状态".to_string()),
        ..window::settings()
    };
    
    let open_task = window::open(window_config);
    
    self.historical_data_status_window = Some(window_id);
    self.historical_data_status_state = Some(state);
    
    return open_task.map(|_| Message::HistoricalDataStatusWindowOpened(window_id));
}
```

#### 7.3.2 菜单集成

**在 Sidebar 中添加菜单项**：
```rust
// 在设置菜单或工具菜单中添加
button("历史数据状态")
    .on_press(Message::OpenHistoricalDataStatusWindow)
```

### 7.4 UI 组件设计

#### 7.4.1 数据表格

**使用 iced 的 table 组件或自定义表格**：
```rust
pub fn view_table(&self) -> Element<Message> {
    let mut rows = Vec::new();
    
    for row in &self.data_rows {
        rows.push(self.view_table_row(row));
    }
    
    table(rows)
        .header(self.view_table_header())
        .into()
}
```

#### 7.4.2 状态徽章

**状态显示组件**：
```rust
pub fn view_status_badge(status: DataAvailability) -> Element<Message> {
    let (icon, color, text) = match status {
        DataAvailability::Available => ("✅", Color::from_rgb(0.0, 0.8, 0.0), "可用"),
        DataAvailability::Downloading => ("⏳", Color::from_rgb(0.0, 0.5, 1.0), "下载中"),
        DataAvailability::Unavailable => ("❌", Color::from_rgb(1.0, 0.0, 0.0), "不可用"),
        DataAvailability::Partial => ("⚠️", Color::from_rgb(1.0, 0.8, 0.0), "部分"),
        DataAvailability::Unknown => ("❓", Color::from_rgb(0.5, 0.5, 0.5), "未知"),
    };
    
    row![
        text(icon).size(16),
        text(text).style(color)
    ]
    .spacing(4)
    .into()
}
```

#### 7.4.3 进度条

**下载进度显示**：
```rust
pub fn view_progress(progress: f32) -> Element<Message> {
    progress_bar(0.0..=1.0, progress)
        .width(Length::Fill)
        .into()
}
```

### 7.5 数据获取

**从 DataAvailabilityIndex 获取数据**：
```rust
impl HistoricalDataStatusWindow {
    async fn refresh_data(&mut self) -> Result<(), DataError> {
        // 1. 获取所有交易对
        let symbols = self.availability_index.get_all_symbols().await;
        
        // 2. 获取每个交易对的所有日期状态
        let mut rows = Vec::new();
        for symbol in symbols {
            let dates = self.availability_index.get_dates_for_symbol(&symbol).await;
            for date in dates {
                let status = self.availability_index.check_availability(&symbol, &date).await;
                let file_size = self.get_file_size(&symbol, &date).await;
                let last_checked = self.availability_index.get_last_checked(&symbol, &date).await;
                
                rows.push(DataStatusRow {
                    symbol: symbol.clone(),
                    date,
                    status,
                    file_size,
                    last_checked,
                    download_progress: None,
                });
            }
        }
        
        // 3. 应用筛选和搜索
        self.data_rows = self.apply_filters(rows);
        
        Ok(())
    }
}
```

### 7.6 性能优化

**虚拟滚动**：
- 如果数据量大（> 1000 行），使用虚拟滚动
- 只渲染可见区域的行

**延迟加载**：
- 文件大小等信息延迟加载
- 避免一次性加载所有数据

**缓存**：
- 缓存数据状态，减少重复查询
- 定期刷新缓存

---

## 八、迁移计划

### 8.1 向后兼容

**现有代码**：
- 保持现有接口不变
- 新功能通过可选参数添加

**数据格式**：
- 缓存文件格式不变
- 新增数据可用性索引文件

### 8.2 渐进式部署

**阶段一**：
- 部署核心基础功能
- 监控系统运行情况

**阶段二**：
- 部署预加载功能
- 逐步启用预加载

**阶段三**：
- 部署高级特性
- 优化系统性能

---

## 九、监控和日志

### 9.1 关键指标

**下载服务**：
- 任务队列长度
- 下载成功率
- 平均下载时间
- 重试次数

**预加载服务**：
- 预加载任务数
- 预加载命中率
- 资源使用情况

**数据可用性**：
- 数据完整性比例
- 缺失数据统计

### 9.2 日志级别

**DEBUG**：
- 任务提交
- 状态更新
- 预加载触发

**INFO**：
- 下载完成
- 数据可用性变化
- 预加载统计

**WARN**：
- 下载失败（临时错误）
- 重试触发

**ERROR**：
- 系统错误
- 数据损坏

---

## 十二、总结

### 10.1 设计优势

1. **职责清晰**：每个服务职责单一，易于维护
2. **非阻塞**：下载不阻塞数据读取，用户体验好
3. **可扩展**：易于添加新功能（如数据质量监控）
4. **事件驱动**：组件解耦，架构清晰
5. **智能预加载**：提升用户体验，减少等待时间
6. **数据可用性感知**：避免无效操作，节省资源

### 10.2 实施优先级

**阶段一（核心基础）**：立即实施
- 解决数据完整性问题的核心机制
- 历史数据状态窗口（基础功能）
- 影响：高（解决核心问题 + 可视化）

**阶段二（智能预加载 + 窗口完善）**：短期实施
- 智能预加载服务
- 历史数据状态窗口功能完善（筛选、搜索、批量操作、统计）
- 影响：中（改善体验 + 功能完善）

**阶段三（高级特性）**：长期优化
- 架构优化和高级功能
- 影响：低（优化和扩展）

### 10.3 预期效果

**阶段一完成后**：
- 数据完整性显著提升
- VP 计算成功率提高
- 系统资源使用更合理
- 历史数据状态可视化（基础功能）

**阶段二完成后**：
- 用户等待时间减少
- 用户体验明显改善
- 系统智能化程度提升
- 数据状态窗口功能完善（筛选、搜索、批量操作、统计）

**阶段三完成后**：
- 架构更加优雅
- 支持更多高级功能
- 系统可维护性提升

---

## 附录

### A. 相关文档

- `data_architecture_design_spec.md` - 数据架构设计规范
- `architecture_and_naming_review.md` - 架构和命名审查

### B. 参考实现

- Lambda 架构设计模式
- 事件驱动架构
- 优先级队列实现
- 指数退避重试策略

### C. 待讨论问题

1. 数据可用性索引的过期策略（多久后重新检查 Unavailable 状态）
2. 预加载的精确范围（前后各多少天）
3. 资源限制的具体数值（并发数、节流间隔）
4. 事件系统的具体实现（使用 tokio::broadcast 还是其他）

---

**文档版本**：1.0  
**最后更新**：2025-11-25  
**作者**：系统架构设计团队

