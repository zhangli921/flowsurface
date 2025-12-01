# 数据共享架构设计提案

## 问题分析

当前架构存在的问题：
1. **数据重复存储**：每个图表实例独立存储相同数据
2. **请求重复**：多个图表同时请求相同数据时，可能重复下载
3. **缺乏共享机制**：没有统一的数据管理层

## 设计目标

1. **单一数据源**：相同的数据只存储一份
2. **请求去重**：相同请求只执行一次
3. **自动缓存**：智能缓存管理，自动清理
4. **类型安全**：利用 Rust 的类型系统保证安全性
5. **零拷贝**：尽可能使用引用而非克隆

## 架构设计

### 1. 核心组件

```
┌─────────────────────────────────────────────────────────┐
│              DataSourceManager (全局单例)                │
│  ┌──────────────────────────────────────────────────┐  │
│  │  RequestDeduplicator: 去重请求                   │  │
│  │  └─ 相同请求合并为一个任务                        │  │
│  └──────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────┐  │
│  │  DataCache: 共享数据缓存                         │  │
│  │  └─ Arc<Data> + 引用计数                         │  │
│  └──────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────┐  │
│  │  EventBus: 事件总线                              │  │
│  │  └─ 数据更新通知所有订阅者                        │  │
│  └──────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
         │                    │                    │
         ▼                    ▼                    ▼
    ┌────────┐          ┌────────┐          ┌────────┐
    │Footprint│          │Candles │          │Heatmap │
    │  Chart  │          │ Chart  │          │ Chart  │
    └────────┘          └────────┘          └────────┘
```

### 2. 数据流

```
请求阶段：
  Chart A 请求 Data(1000-2000)
    └─> DataSourceManager::request()
        ├─> 检查缓存 → 命中 → 返回 Arc<Data>
        └─> 未命中 → 检查 pending_requests
            ├─> 已有请求 → 添加到订阅列表
            └─> 新请求 → 创建下载任务

下载阶段：
  Download Task 完成
    └─> DataSourceManager::on_fetched()
        ├─> 更新缓存
        └─> 通过 EventBus 通知所有订阅者

使用阶段：
  所有订阅者收到通知
    └─> 更新各自的视图
    └─> 数据通过 Arc 共享，零拷贝
```

## 实现方案

### 方案 A: 基于 Arc 的共享数据（推荐）

**优点**：
- 零拷贝，内存高效
- Rust 原生支持，类型安全
- 自动引用计数，无需手动管理

**实现**：

```rust
// 1. 数据键
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct DataKey {
    pub ticker: TickerInfo,
    pub range: FetchRange,
}

// 2. 共享数据缓存
pub struct SharedDataCache {
    cache: Arc<RwLock<HashMap<DataKey, Arc<CachedData>>>>,
    pending: Arc<RwLock<HashMap<DataKey, Vec<SubscriberId>>>>,
}

struct CachedData {
    data: FetchedData,
    subscribers: Vec<SubscriberId>,
    timestamp: Instant,
}

// 3. 数据源管理器
pub struct DataSourceManager {
    cache: SharedDataCache,
    event_bus: EventBus,
}

impl DataSourceManager {
    pub fn request_data(
        &self,
        key: DataKey,
        subscriber: SubscriberId,
    ) -> RequestResult {
        // 检查缓存
        if let Some(cached) = self.cache.get(&key) {
            return RequestResult::Cached(Arc::clone(cached));
        }
        
        // 检查是否有进行中的请求
        if self.cache.add_subscriber(&key, subscriber) {
            return RequestResult::Pending;
        }
        
        // 新请求
        RequestResult::NewRequest(key)
    }
    
    pub fn on_data_fetched(&self, key: DataKey, data: FetchedData) {
        let subscribers = self.cache.store(&key, data);
        self.event_bus.notify(key, subscribers);
    }
}
```

### 方案 B: 基于消息通道的事件驱动

**优点**：
- 完全解耦
- 易于扩展
- 支持异步处理

**实现**：

```rust
// 使用 tokio::sync::broadcast 或 crossbeam::channel
pub struct DataEventBus {
    sender: broadcast::Sender<DataEvent>,
}

pub enum DataEvent {
    DataFetched { key: DataKey, data: Arc<FetchedData> },
    DataUpdated { key: DataKey, data: Arc<FetchedData> },
}

// 图表订阅事件
impl KlineChart {
    fn subscribe_to_data(&mut self, key: DataKey) {
        let mut receiver = self.data_manager.event_bus.subscribe();
        // 在异步任务中接收事件
        self.event_task = Some(spawn(async move {
            while let Ok(event) = receiver.recv().await {
                match event {
                    DataEvent::DataFetched { key: k, data } if k == key => {
                        self.on_data_received(data);
                    }
                    _ => {}
                }
            }
        }));
    }
}
```

### 方案 C: 混合方案（最佳实践）

结合方案 A 和 B：
- 使用 Arc 共享数据（零拷贝）
- 使用事件总线通知更新（解耦）
- 请求去重在 DataSourceManager 层统一处理

## 集成到现有代码

### 1. 修改 Dashboard 结构

```rust
pub struct Dashboard {
    // ... 现有字段
    data_manager: Arc<DataSourceManager>,  // 新增
}
```

### 2. 修改 KlineChart

```rust
pub struct KlineChart {
    // ... 现有字段
    data_subscription: Option<DataSubscription>,  // 新增
    shared_data: Option<Arc<FetchedData>>,        // 新增：共享数据引用
}

impl KlineChart {
    pub fn request_data(&mut self, range: FetchRange) {
        let key = DataKey {
            ticker: self.chart.ticker_info,
            range,
        };
        
        match self.data_manager.request_data(key, self.id) {
            RequestResult::Cached(data) => {
                // 直接使用缓存数据
                self.shared_data = Some(data);
                self.invalidate(None);
            }
            RequestResult::Pending => {
                // 等待数据到达
            }
            RequestResult::NewRequest(key) => {
                // 发起新请求
                self.data_manager.fetch_data(key);
            }
        }
    }
}
```

### 3. 统一请求处理

```rust
// 在 Dashboard 层统一处理请求
impl Dashboard {
    fn handle_fetch_request(&mut self, req: FetchRequest) {
        let key = DataKey::from(&req);
        
        // 检查是否已有相同请求
        if let Some(existing_task) = self.data_manager.get_pending_task(&key) {
            // 复用现有任务
            return;
        }
        
        // 创建新任务
        let task = self.create_fetch_task(key);
        self.data_manager.register_task(key, task);
    }
}
```

## 优势总结

1. **内存效率**：相同数据只存储一份，通过 Arc 共享
2. **网络效率**：相同请求只执行一次
3. **代码清晰**：职责分离，易于维护
4. **易于扩展**：新图表类型可以轻松接入
5. **类型安全**：Rust 类型系统保证安全性

## 迁移路径

1. **阶段 1**：实现 DataSourceManager，保持向后兼容
2. **阶段 2**：逐步迁移现有图表使用新 API
3. **阶段 3**：移除旧的 RequestHandler，统一使用新架构

## 注意事项

1. **线程安全**：使用 Arc<RwLock<>> 或 Arc<Mutex<>>
2. **生命周期**：确保数据在订阅者存在期间不被释放
3. **内存泄漏**：及时清理无订阅者的缓存项
4. **性能**：缓存大小限制，LRU 淘汰策略

