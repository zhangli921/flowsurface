# 渲染层和交互层设计报告 V2（优化版）

## 文档说明

本文档是经过深入分析现有架构后的优化设计方案，**充分利用现有机制，避免重复设计**。

**关键发现**：现有系统已经有一个完整的数据获取机制，我们应该**扩展现有机制**而不是创建新的。

---

## 一、现有架构分析

### 1.1 现有的数据获取流程

```
Chart::invalidate() 
    ↓ 返回 Action::RequestFetch
Dashboard::tick()
    ↓ 处理 Action::RequestFetch
request_fetch()
    ↓ 调用 kline_fetch_task()
kline_fetch_task()
    ↓ 使用 adapter::fetch_klines (旧系统)
    ↓ 返回 FetchedData::Klines
DistributeFetchedData
    ↓ 调用 insert_hist_klines()
Chart::insert_hist_klines()
    ↓ 存储数据并触发 VP 计算
```

### 1.2 现有机制的优势

✅ **完整的请求-响应流程**：
- `chart::Action::RequestFetch` 已存在
- `FetchedData::Klines` 已存在
- `insert_hist_klines` 已存在
- 请求 ID 管理（`RequestHandler`）已存在

✅ **统一的错误处理**：
- 通过 `FetchedData` 统一处理
- 状态管理（`pane::Status::Loading`）已存在

✅ **VP 计算集成**：
- `insert_hist_klines` 后自动触发 VP 计算

### 1.3 需要改进的地方

❌ **数据源问题**：
- `kline_fetch_task` 使用 `adapter::fetch_klines`（旧系统）
- 需要改为使用 `UnifiedDataService`

❌ **可见范围检测缺失**：
- `invalidate` 方法未检测可见范围变化
- 需要添加检测逻辑

❌ **数据格式转换**：
- `UnifiedDataService` 返回 `data::kline::KLine`
- `insert_hist_klines` 需要 `exchange::Kline`
- 需要转换逻辑

---

## 二、优化后的设计方案

### 2.1 核心设计原则

1. **复用现有机制**：不创建新的消息类型，扩展现有 `RequestFetch` 机制
2. **最小改动**：只修改必要的地方，保持架构一致性
3. **渐进式迁移**：先让新系统工作，再逐步移除旧代码

### 2.2 数据流设计（优化后）

```
用户交互（拖动/缩放/切换）
    ↓
Chart::invalidate() 检测可见范围变化
    ↓ 返回 Action::RequestFetch (现有机制)
Dashboard::tick() 处理 Action
    ↓
request_fetch() → kline_fetch_task() (修改：使用 UnifiedDataService)
    ↓
UnifiedDataService 自动选择数据源（实时/历史）
    ↓
数据返回 FetchedData::Klines (现有机制)
    ↓
insert_hist_klines() (现有机制，添加数据转换)
    ↓
存储到 ChartState 并触发 VP 计算
```

### 2.3 关键修改点

#### 修改点 1：`kline_fetch_task` 使用 UnifiedDataService

**位置**：`flowsurface/src/screen/dashboard.rs`

**当前代码**：
```rust
fn kline_fetch_task(...) -> Task<Message> {
    Task::perform(
        adapter::fetch_klines(ticker_info, timeframe, range)  // ❌ 旧系统
            .map_err(|err| err.to_user_message()),
        ...
    )
}
```

**修改后**：
```rust
fn kline_fetch_task(
    layout_id: uuid::Uuid,
    pane_id: uuid::Uuid,
    stream: StreamKind,
    req_id: Option<uuid::Uuid>,
    range: Option<(u64, u64)>,
    unified_service: Arc<UnifiedDataService>,  // 新增参数
) -> Task<Message> {
    let update_status = Task::done(Message::ChangePaneStatus(...));
    
    let fetch_task = match stream {
        StreamKind::Kline { ticker_info, timeframe } => {
            let symbol = ticker_info.ticker.to_string();
            let timeframe_str = timeframe.to_string();
            
            // 构建 TimeRange
            let time_range = range.map(|(from, to)| {
                data::TimeRange {
                    start_us: from * 1_000,  // 毫秒转微秒
                    end_us: to * 1_000,
                }
            }).unwrap_or_else(|| {
                // 如果没有指定范围，使用默认范围（例如：最近24小时）
                let now = chrono::Utc::now().timestamp_micros() as u64;
                data::TimeRange {
                    start_us: now - 24 * 3600 * 1_000_000,  // 24小时前
                    end_us: now,
                }
            });
            
            Task::perform(
                async move {
                    unified_service
                        .fetch_klines(symbol, time_range, &timeframe_str)
                        .await
                        .map_err(|e| e.to_string())
                },
                move |result| match result {
                    Ok(klines) => {
                        // 转换数据格式：data::kline::KLine -> exchange::Kline
                        let exchange_klines: Vec<exchange::Kline> = klines
                            .iter()
                            .map(|k| convert_to_exchange_kline(k))
                            .collect();
                        
                        let data = FetchedData::Klines {
                            data: exchange_klines,
                            req_id,
                        };
                        Message::DistributeFetchedData {
                            layout_id,
                            pane_id,
                            data,
                            stream,
                        }
                    }
                    Err(err) => {
                        Message::ErrorOccurred(
                            Some(pane_id),
                            DashboardError::Fetch(err)
                        )
                    }
                },
            )
        }
        _ => Task::none(),
    };
    
    update_status.chain(fetch_task)
}
```

**注意**：需要将 `unified_service` 传递给 `kline_fetch_task`。可以通过以下方式：
- 在 `Dashboard` 中存储 `Arc<UnifiedDataService>`
- 在 `request_fetch` 中传递

#### 修改点 2：在 `invalidate` 中检测可见范围变化

**位置**：`flowsurface/src/chart/kline.rs`

**当前代码**：
```rust
pub fn invalidate(&mut self, now: Option<Instant>) -> Option<Action> {
    // ... 现有逻辑 ...
    // ❌ 没有检测可见范围变化
}
```

**修改后**：
```rust
pub fn invalidate(&mut self, now: Option<Instant>) -> Option<Action> {
    // ... 现有逻辑 ...
    
    // 检测可见范围变化，如果需要新数据，返回 RequestFetch
    if let Some(action) = self.check_data_update_needed() {
        return Some(action);
    }
    
    // ... 其他逻辑 ...
}

fn check_data_update_needed(&mut self) -> Option<Action> {
    // 防抖：限制请求频率
    if self.last_data_request.elapsed().as_millis() < 500 {
        return None;
    }
    
    // 1. 获取可见时间范围
    let (from_ms, to_ms) = self.visible_timerange()?;
    
    // 2. 检查是否需要新数据
    if !self.has_data_for_range(from_ms, to_ms) {
        // 3. 获取 symbol 和 timeframe
        let symbol = self.chart.state.ticker_info.ticker.to_string();
        let timeframe = match self.chart.state.basis {
            Basis::Time(tf) => tf,
            Basis::Tick(_) => return None,  // Tick-based 不支持
        };
        
        // 4. 构建 FetchRequest
        let req_id = self.request_handler.new_request();
        let fetch_range = exchange::fetcher::FetchRange::Kline(from_ms, to_ms);
        let stream = exchange::StreamKind::Kline {
            ticker_info: self.chart.state.ticker_info.clone(),
            timeframe,
        };
        
        self.last_data_request = Instant::now();
        
        return Some(Action::RequestFetch(
            exchange::fetcher::FetchRequests::new(vec![
                exchange::fetcher::FetchRequest {
                    req_id,
                    fetch: fetch_range,
                    stream: Some(stream),
                }
            ])
        ));
    }
    
    None
}

fn has_data_for_range(&self, from_ms: u64, to_ms: u64) -> bool {
    match &self.data_source {
        PlotData::TimeBased(timeseries) => {
            if timeseries.datapoints.is_empty() {
                return false;
            }
            
            // 获取数据的时间范围
            let data_start = *timeseries.datapoints.keys().next()?;
            let data_end = *timeseries.datapoints.keys().last()?;
            
            // 检查请求范围是否在数据范围内（考虑缓冲）
            let buffer = 60 * 1_000;  // 1分钟缓冲（毫秒）
            data_start <= from_ms.saturating_sub(buffer) &&
            data_end >= to_ms.saturating_add(buffer)
        }
        PlotData::TickBased(_) => false,
    }
}
```

**新增字段**：
```rust
pub struct KlineChart {
    // ... 现有字段 ...
    last_data_request: Instant,  // 新增：用于防抖
}
```

#### 修改点 3：数据格式转换函数

**位置**：`flowsurface/src/chart/kline.rs` 或新建工具模块

**实现**：
```rust
fn convert_to_exchange_kline(k: &data::kline::KLine) -> exchange::Kline {
    exchange::Kline {
        time: k.open_time_us / 1_000,  // 微秒转毫秒
        open: exchange::util::Price::from_f64(k.open),
        high: exchange::util::Price::from_f64(k.high),
        low: exchange::util::Price::from_f64(k.low),
        close: exchange::util::Price::from_f64(k.close),
        volume: (
            (k.volume * 1_000_000.0) as u64,  // 转换为整数
            0
        ),
    }
}
```

---

## 三、架构对比

### 3.1 原设计方案（V1）的问题

❌ **创建了新的消息类型**：
- `Message::FetchKLines` - 与现有机制重复
- `Message::KLineDataFetched` - 与现有机制重复
- `Action::RequestFetchKLines` - 与现有 `Action::RequestFetch` 重复

❌ **绕过了现有机制**：
- 没有利用现有的 `RequestHandler`
- 没有利用现有的错误处理流程
- 增加了代码复杂度

### 3.2 优化方案（V2）的优势

✅ **复用现有机制**：
- 使用现有的 `Action::RequestFetch`
- 使用现有的 `FetchedData::Klines`
- 使用现有的 `insert_hist_klines`

✅ **最小改动**：
- 只修改 `kline_fetch_task` 的数据源
- 只添加可见范围检测逻辑
- 保持架构一致性

✅ **渐进式迁移**：
- 新系统可以工作
- 旧代码可以逐步移除
- 降低风险

---

## 四、实施步骤

### Phase 1: 数据源切换（优先级：高）

1. ✅ 在 `Dashboard` 中存储 `Arc<UnifiedDataService>`
2. ✅ 修改 `kline_fetch_task` 使用 `UnifiedDataService`
3. ✅ 实现数据转换函数
4. ✅ 测试数据获取和存储

### Phase 2: 可见范围检测（优先级：高）

1. ✅ 添加 `last_data_request` 字段
2. ✅ 实现 `check_data_update_needed` 方法
3. ✅ 实现 `has_data_for_range` 方法
4. ✅ 在 `invalidate` 中集成检测逻辑
5. ✅ 测试可见范围变化触发

### Phase 3: Symbol 和 Timeframe 切换（优先级：中）

1. ✅ 在 `set_basis` 中触发数据获取（如果已有数据不足）
2. ✅ 在 symbol 切换时触发数据获取
3. ✅ 测试切换功能

### Phase 4: 清理和优化（优先级：低）

1. ✅ 移除旧的 `adapter::fetch_klines` 调用
2. ✅ 优化防抖机制
3. ✅ 性能测试

---

## 五、关键设计决策

### 5.1 为什么复用现有机制？

**原因**：
1. **一致性**：保持代码库的一致性，降低维护成本
2. **可靠性**：现有机制已经过测试，风险更低
3. **简洁性**：避免重复代码，减少复杂度

### 5.2 为什么在 `invalidate` 中检测？

**原因**：
1. **时机合适**：`invalidate` 在用户交互时被调用
2. **已有机制**：`invalidate` 已经返回 `Option<Action>`
3. **统一入口**：所有需要数据更新的场景都会经过这里

### 5.3 为什么需要数据转换？

**原因**：
1. **格式差异**：`data::kline::KLine` 使用微秒，`exchange::Kline` 使用毫秒
2. **类型差异**：价格类型不同（`f64` vs `Price`）
3. **兼容性**：保持与现有 `insert_hist_klines` 的兼容

---

## 六、风险评估

### 6.1 技术风险

**风险 1**：数据转换可能丢失精度
- **缓解**：仔细测试转换逻辑，确保精度损失可接受

**风险 2**：可见范围检测可能不准确
- **缓解**：添加缓冲机制，多次测试验证

**风险 3**：与现有代码冲突
- **缓解**：仔细检查现有代码，确保兼容性

### 6.2 集成风险

**风险 1**：`kline_fetch_task` 的调用点需要传递 `unified_service`
- **缓解**：在 `Dashboard` 中存储，通过参数传递

**风险 2**：时间单位转换（毫秒 vs 微秒）可能出错
- **缓解**：使用明确的转换函数，添加注释

---

## 七、总结

### 7.1 核心改进

1. **复用现有机制**：不创建新的消息类型
2. **最小改动**：只修改必要的地方
3. **渐进式迁移**：降低风险

### 7.2 关键优势

✅ **架构一致性**：与现有代码保持一致
✅ **代码简洁**：避免重复代码
✅ **风险可控**：最小改动，易于测试

### 7.3 下一步

等待评审后开始实施 Phase 1。

