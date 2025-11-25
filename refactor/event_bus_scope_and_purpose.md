# 事件总线的作用和适用范围

## 一、设计意图

事件总线被设计为**统一的数据服务层事件总线**（Unified event bus for data service layer），而不是仅限于历史数据下载。

### 1.1 设计目标

从代码注释可以看出：
```rust
//! Unified event bus for data service layer.
//!
//! This module provides a centralized event system for communication between
//! data services, allowing components to publish and subscribe to events
//! without tight coupling.
```

**核心目标**：
- 为**整个数据服务层**提供统一的事件通信机制
- 解耦数据服务组件之间的通信
- 支持多个订阅者（UI、日志、监控等）

### 1.2 数据服务层架构

数据服务层包括以下组件：

```
数据服务层
├── UnifiedDataService（统一数据服务）
├── RealtimeDataService（实时数据读取）
├── HistoricalDataService（历史数据读取）
├── RealtimeIngesterService（实时数据写入）
├── HistoricalIngesterService（历史数据下载）
└── VpComputeService（VP计算服务）
```

**事件总线应该服务于所有这些组件**，而不仅仅是历史数据下载。

---

## 二、当前实现 vs 设计意图

### 2.1 当前实现的事件类型

目前定义的事件类型主要围绕**历史数据下载**：

```rust
pub enum DataEvent {
    // ========== Download Events ==========
    DownloadRequested { ... }
    DownloadStarted { ... }
    DownloadProgress { ... }
    DownloadCompleted { ... }
    DownloadFailed { ... }
    
    // ========== Availability Events ==========
    AvailabilityChanged { ... }
    
    // ========== Data Events ==========
    DataUpdated { ... }
}
```

**原因**：
- 阶段一的目标是解决历史数据完整性问题
- 当前最迫切的需求是历史数据下载的状态通知
- 遵循"按需实现"的原则

### 2.2 设计意图（未来扩展）

事件总线应该支持**所有数据服务层的事件**，包括但不限于：

#### 实时数据事件
```rust
// 实时数据事件（未来可以添加）
RealtimeDataStarted { symbol: String },
RealtimeDataStopped { symbol: String },
RealtimeDataError { symbol: String, error: String },
RealtimeKlineUpdated { symbol: String, kline: KLine },
```

#### VP计算事件
```rust
// VP计算事件（未来可以添加）
VpComputeStarted { symbol: String, range: TimeRange },
VpComputeCompleted { symbol: String, range: TimeRange },
VpComputeFailed { symbol: String, range: TimeRange, error: String },
```

#### 缓存事件
```rust
// 缓存事件（未来可以添加）
CacheHit { symbol: String, date: String },
CacheMiss { symbol: String, date: String },
CacheInvalidated { symbol: String, date: String },
```

#### 数据质量事件
```rust
// 数据质量事件（未来可以添加）
DataIncomplete { symbol: String, range: TimeRange, coverage: f32 },
DataCorrupted { symbol: String, date: String },
DataValidated { symbol: String, date: String },
```

---

## 三、事件总线的核心作用

### 3.1 解耦组件通信

**问题**：组件之间直接依赖会导致：
- 紧耦合（难以测试、难以替换）
- 循环依赖风险
- 难以扩展

**解决方案**：通过事件总线，组件之间通过事件通信，不直接依赖。

**示例**：
```rust
// ❌ 紧耦合（直接依赖）
struct HistoricalDataService {
    download_service: Arc<HistoricalDownloadService>, // 直接依赖
}

// ✅ 解耦（通过事件通信）
struct HistoricalDataService {
    event_bus: Arc<EventBus>, // 只依赖事件总线
}

// HistoricalDownloadService 发布事件
event_bus.publish(DataEvent::DownloadCompleted { ... });

// HistoricalDataService 订阅事件
let mut receiver = event_bus.subscribe();
```

### 3.2 支持多个订阅者

**优势**：一个事件可以被多个组件同时订阅和处理。

**示例场景**：
```rust
// 下载完成事件可以被多个组件订阅：
// 1. HistoricalDataStatusWindow（更新UI）
event_bus.subscribe() -> 更新状态窗口

// 2. 日志系统（记录日志）
event_bus.subscribe() -> 写入日志文件

// 3. 监控系统（收集指标）
event_bus.subscribe() -> 更新下载成功率指标

// 4. 缓存系统（触发缓存刷新）
event_bus.subscribe() -> 刷新相关缓存
```

### 3.3 实时UI更新

**优势**：UI组件可以实时响应数据状态变化，无需轮询。

**示例**：
```rust
// HistoricalDataStatusWindow 订阅事件
impl HistoricalDataStatusWindow {
    pub fn subscription(&self) -> Subscription<Message> {
        let mut receiver = self.event_bus.subscribe();
        iced::subscription::run_with_id("data_events", async move {
            while let Ok(event) = receiver.recv().await {
                yield Message::DataEvent(event);
            }
        })
    }
    
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::DataEvent(DataEvent::DownloadCompleted { symbol, date, .. }) => {
                // 实时更新UI：显示下载完成状态
                self.update_row_status(&symbol, &date, DataAvailability::Available);
            }
            // ...
        }
    }
}
```

---

## 四、适用范围总结

### 4.1 当前阶段（阶段一）

**主要用途**：
- ✅ 历史数据下载状态通知
- ✅ 数据可用性状态变化
- ✅ 数据更新通知

**使用场景**：
- HistoricalDownloadService 发布下载事件
- HistoricalDataStatusWindow 订阅事件并更新UI
- 日志系统记录事件（可选）

### 4.2 未来扩展

**可以扩展到**：
- ✅ 实时数据服务事件（RealtimeDataService、RealtimeIngesterService）
- ✅ VP计算服务事件（VpComputeService）
- ✅ 缓存管理事件
- ✅ 数据质量监控事件
- ✅ 性能指标事件

**设计原则**：
- 事件总线是**统一的基础设施**，不限于特定功能
- 按需添加新的事件类型
- 保持事件类型的清晰分类（下载、可用性、数据、计算等）

---

## 五、最佳实践

### 5.1 何时使用事件总线

**应该使用事件总线**：
- ✅ 状态变化通知（下载开始/完成、数据可用性变化）
- ✅ 跨组件通信（不需要直接依赖）
- ✅ 需要多个订阅者的场景（UI、日志、监控）
- ✅ 异步通知（不阻塞调用者）

**不应该使用事件总线**：
- ❌ 同步操作（需要立即返回结果）
- ❌ 错误处理（应该使用 Result 类型）
- ❌ 数据传递（应该使用函数参数或返回值）

### 5.2 事件类型设计原则

1. **清晰分类**：按功能分类（下载、可用性、数据、计算等）
2. **包含足够信息**：事件应该包含处理所需的所有信息
3. **避免过度细化**：不要为每个小操作都创建事件
4. **保持向后兼容**：添加新事件类型时，保持现有事件不变

---

## 六、总结

### 6.1 事件总线的作用

1. **统一通信机制**：为整个数据服务层提供统一的事件通信
2. **解耦组件**：组件之间通过事件通信，不直接依赖
3. **支持多订阅者**：一个事件可以被多个组件订阅和处理
4. **实时UI更新**：UI可以实时响应数据状态变化

### 6.2 适用范围

- **当前**：主要用于历史数据下载（阶段一需求）
- **未来**：扩展到所有数据服务层组件（实时数据、VP计算、缓存管理等）

### 6.3 设计原则

- **统一基础设施**：事件总线是数据服务层的基础设施，不限于特定功能
- **按需扩展**：根据实际需求添加新的事件类型
- **保持清晰**：事件类型按功能分类，保持代码清晰

---

**结论**：事件总线是为**整个数据服务层**设计的统一通信机制，当前实现主要围绕历史数据下载是因为这是阶段一的需求。未来可以根据需要扩展到实时数据、VP计算、缓存管理等所有数据服务层组件。

