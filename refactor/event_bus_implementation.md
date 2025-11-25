# 事件总线实施完成

## 概述

阶段一：添加统一事件总线系统已经完成。事件总线为数据服务层提供了统一的通信机制，允许组件之间通过事件进行解耦通信。

## 实施内容

### 1. 创建的事件总线模块

**文件**：`flowsurface/data/src/event_bus.rs`

**核心组件**：
- `EventBus`：统一事件总线，基于 `tokio::sync::broadcast`
- `DataEvent`：统一事件类型枚举
- `DataType`：数据类型（Tick、Kline）
- `DownloadPriority`：下载优先级（UserRequest、Preload、Background）
- `DataAvailability`：数据可用性状态（Available、Downloading、Unavailable、Partial、Unknown）

### 2. 事件类型

#### 下载事件
- `DownloadRequested`：下载任务已请求
- `DownloadStarted`：下载已开始
- `DownloadProgress`：下载进度更新（0.0 - 1.0）
- `DownloadCompleted`：下载完成
- `DownloadFailed`：下载失败

#### 可用性事件
- `AvailabilityChanged`：数据可用性状态变化

#### 数据事件
- `DataUpdated`：数据已更新（缓存中有新数据）

### 3. 集成到主应用

**文件**：`flowsurface/src/main.rs`

**修改点**：
1. 在 `Flowsurface` 结构体中添加 `event_bus: Arc<data::EventBus>`
2. 在 `new()` 方法中初始化事件总线
3. 在 `Message` 枚举中添加 `DataEvent(data::DataEvent)`
4. 在 `update()` 方法中添加事件处理逻辑（目前仅记录日志）

## 使用方法

### 发布事件

```rust
use data::event_bus::{EventBus, DataEvent, DataType, DownloadPriority};

let event_bus = Arc::new(EventBus::new());

// 发布下载开始事件
event_bus.publish(DataEvent::DownloadStarted {
    symbol: "BTCUSDT".to_string(),
    date: "2025-11-25".to_string(),
    data_type: DataType::Tick,
}).unwrap();
```

### 订阅事件

```rust
use data::event_bus::{EventBus, DataEvent};

let event_bus = Arc::new(EventBus::new());
let mut receiver = event_bus.subscribe();

// 在异步上下文中接收事件
tokio::spawn(async move {
    while let Ok(event) = receiver.recv().await {
        match event {
            DataEvent::DownloadStarted { symbol, date, .. } => {
                println!("Download started: {} {}", symbol, date);
            }
            DataEvent::DownloadCompleted { symbol, date, .. } => {
                println!("Download completed: {} {}", symbol, date);
            }
            _ => {}
        }
    }
});
```

## 下一步

### 当 HistoricalDownloadService 实现时

需要在 `HistoricalDownloadService` 中：
1. 接收 `EventBus` 作为依赖
2. 在下载开始时发布 `DownloadStarted` 事件
3. 在下载完成时发布 `DownloadCompleted` 事件
4. 在下载失败时发布 `DownloadFailed` 事件
5. 在更新索引时发布 `AvailabilityChanged` 事件

**示例代码**：
```rust
impl HistoricalDownloadService {
    pub fn new(
        availability_index: Arc<DataAvailabilityIndex>,
        ingester: Arc<HistoricalIngesterService>,
        event_bus: Arc<EventBus>, // 添加事件总线
    ) -> Self {
        // ...
    }
    
    async fn download_with_retry(&self, task: DownloadTask) -> Result<(), DataError> {
        // 发布下载开始事件
        self.event_bus.publish(DataEvent::DownloadStarted {
            symbol: task.symbol.clone(),
            date: task.date.clone(),
            data_type: task.data_type.clone(),
        }).ok();
        
        // 执行下载...
        
        match result {
            Ok(_) => {
                // 发布下载完成事件
                self.event_bus.publish(DataEvent::DownloadCompleted {
                    symbol: task.symbol.clone(),
                    date: task.date.clone(),
                    data_type: task.data_type.clone(),
                }).ok();
            }
            Err(e) => {
                // 发布下载失败事件
                self.event_bus.publish(DataEvent::DownloadFailed {
                    symbol: task.symbol.clone(),
                    date: task.date.clone(),
                    data_type: task.data_type.clone(),
                    error: e.to_string(),
                    retry_count: task.retry_count,
                }).ok();
            }
        }
    }
}
```

### 当 HistoricalDataStatusWindow 实现时

需要在 `HistoricalDataStatusWindow` 中：
1. 订阅事件总线
2. 在 `subscription()` 方法中创建事件订阅
3. 在 `update()` 方法中处理事件，更新 UI 状态

**示例代码**：
```rust
impl HistoricalDataStatusWindow {
    pub fn subscription(&self) -> Subscription<Message> {
        // 订阅事件总线
        let mut receiver = self.event_bus.subscribe();
        
        iced::subscription::run_with_id(
            "data_event_subscription",
            async move {
                let mut receiver = receiver;
                while let Ok(event) = receiver.recv().await {
                    yield Message::DataEvent(event);
                }
            },
        )
    }
    
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::DataEvent(event) => {
                match event {
                    DataEvent::DownloadStarted { symbol, date, .. } => {
                        // 更新 UI：显示下载中状态
                        self.update_row_status(&symbol, &date, DataAvailability::Downloading);
                    }
                    DataEvent::DownloadCompleted { symbol, date, .. } => {
                        // 更新 UI：显示可用状态
                        self.update_row_status(&symbol, &date, DataAvailability::Available);
                    }
                    DataEvent::DownloadFailed { symbol, date, .. } => {
                        // 更新 UI：显示失败状态
                        self.update_row_status(&symbol, &date, DataAvailability::Unavailable);
                    }
                    _ => {}
                }
            }
            // ... 其他消息处理
        }
    }
}
```

## 优势

1. **解耦**：组件之间通过事件通信，不直接依赖
2. **可扩展**：易于添加新的事件类型和订阅者
3. **实时更新**：UI 可以实时响应数据状态变化
4. **多订阅者**：多个组件可以同时订阅同一事件

## 注意事项

1. **事件丢失**：如果订阅者处理速度慢，可能会丢失事件（broadcast channel 的特性）
2. **内存使用**：事件总线使用固定容量的 channel（默认 1000），如果事件产生速度过快，可能会阻塞
3. **错误处理**：`publish()` 返回 `Result`，但通常可以忽略（如果 channel 关闭，说明应用正在关闭）

## 测试

事件总线模块包含单元测试：
- 基本事件发布和接收
- 多个订阅者
- 订阅者计数

运行测试：
```bash
cd flowsurface/data
cargo test event_bus
```

---

**实施日期**：2025-11-25  
**状态**：✅ 完成  
**下一步**：等待 HistoricalDownloadService 和 HistoricalDataStatusWindow 实现时集成

