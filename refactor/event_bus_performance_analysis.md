# EventBus 性能分析与优化方案

## 当前实现的性能问题

### 1. 性能瓶颈分析

**当前实现（Tick 轮询方式）：**
- **频率**：每 100ms 触发一次 Tick
- **开销**：
  - 每次 Tick 创建异步任务（`Task::perform`）
  - 需要 `lock().await` Mutex（`Arc<Mutex<Receiver>>`）
  - 异步等待 lock 的开销
  - 每次只处理一个事件，其他事件延迟到下一个 Tick（最多 100ms）

**性能影响：**
1. **延迟**：事件处理延迟可达 100ms（Tick 间隔）
2. **阻塞风险**：Mutex lock 可能阻塞，特别是在高并发场景
3. **CPU 开销**：频繁创建异步任务和 lock/unlock 操作
4. **事件积压**：如果事件产生速度 > 10 events/100ms，会积压

### 2. 对金融数据软件的影响

**实时数据流（不受影响）：**
- 市场数据（trades, depth）通过 WebSocket 直接处理
- K-line 更新通过 `MarketWsEvent` 直接处理
- 这些路径不经过 EventBus

**可能受影响的操作：**
- VP 计算状态更新（`VpOverlapThresholdUpdated`）
- 历史数据下载状态（`DownloadStarted`, `DownloadCompleted`）
- 数据可用性更新（`AvailabilityChanged`）

**影响评估：**
- ✅ **低影响**：这些事件本身不是高频的（VP 计算几分钟一次，下载状态更新秒级）
- ⚠️ **潜在问题**：如果同时有大量下载任务，可能产生事件风暴

## 优化方案

### 方案 A：无锁 Channel 桥接（推荐）

**架构：**
```
EventBus (broadcast) 
    ↓ [后台任务持续接收]
mpsc::UnboundedChannel (无锁)
    ↓ [Iced Subscription 直接接收]
UI Message Handler
```

**优势：**
- ✅ 无锁设计，零阻塞
- ✅ 事件即时处理（无 100ms 延迟）
- ✅ 低 CPU 开销（无频繁 lock/unlock）
- ✅ 自动背压（channel 满时阻塞发送端，不影响接收端）

**实现要点：**
1. 在初始化时启动后台任务，持续接收 EventBus 事件
2. 转发到 `mpsc::UnboundedChannel`
3. 使用 Iced `Subscription::run_with` 直接从 channel 接收

### 方案 B：批量处理优化（当前方案的改进）

**改进点：**
1. 移除 Mutex，使用 `Arc<Receiver>`（但 Receiver 不是 Send，需要包装）
2. 在 Tick 中批量处理所有可用事件（而非只处理一个）
3. 使用 `try_recv` 非阻塞接收，避免异步开销

**限制：**
- 仍受 Tick 频率限制（100ms）
- 需要处理 Receiver 的 Send 问题

### 方案 C：混合方案（最佳实践）

**设计：**
- **高频事件**（VP 状态、下载进度）：使用无锁 channel 桥接
- **低频事件**（窗口状态、配置更新）：保持 Tick 轮询

**实现：**
- 为不同类型事件创建不同的 channel
- 根据事件重要性选择处理方式

## 推荐实施：方案 A（无锁 Channel 桥接）

### 性能对比

| 指标 | 当前实现 | 优化后 |
|------|---------|--------|
| 事件延迟 | 0-100ms | <1ms |
| CPU 开销 | 中等（lock/unlock） | 低（无锁） |
| 阻塞风险 | 有（Mutex） | 无 |
| 事件吞吐 | 10 events/100ms | 无限制 |
| 实现复杂度 | 低 | 中 |

### 实施步骤

1. 在 `Flowsurface::new` 中创建 `mpsc::UnboundedChannel`
2. 启动后台任务，持续接收 EventBus 事件并转发
3. 使用 `Subscription::run_with` 创建 subscription
4. 移除 Tick 中的 EventBus 轮询逻辑

### 代码示例

```rust
// 初始化时
let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

// 后台任务
tokio::spawn(async move {
    let mut receiver = event_bus.subscribe();
    loop {
        match receiver.recv().await {
            Ok(event) => {
                if tx.send(event).is_err() {
                    break; // UI 已关闭
                }
            }
            Err(_) => break,
        }
    }
});

// Subscription
let event_subscription = Subscription::run_with(
    (),
    move |_| {
        let mut rx = rx.clone();
        async move {
            rx.recv().await.map(Message::DataEvent)
        }
    },
);
```

## 结论

**当前实现的影响：**
- 对实时市场数据处理：**无影响**（走不同路径）
- 对 VP 计算、下载状态：**轻微延迟**（可接受，非关键路径）

**是否需要优化：**
- 如果事件频率低（<10 events/100ms）：**当前实现足够**
- 如果事件频率高或需要即时响应：**建议实施方案 A**

**建议：**
1. 先监控实际事件频率
2. 如果发现延迟问题，再实施优化
3. 优化优先级：低（不影响核心交易数据流）

