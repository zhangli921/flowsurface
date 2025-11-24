# 渲染层和交互层设计报告

## 文档说明

本文档描述渲染层和交互层需要完成的工作，基于已完成的数据架构重构。

**目标读者**：程序员、架构师、测试人员

**文档目的**：明确渲染层和交互层的实现方案，确保与数据层的正确集成

---

## 一、当前状态分析

### 1.1 已完成的工作

✅ **数据层架构**：
- `UnifiedDataService` 已实现，提供统一的数据访问接口
- `RealtimeDataService` 和 `HistoricalDataService` 已实现
- `RealtimeIngesterService` 和 `HistoricalIngesterService` 已实现
- Binance Data Vision 下载功能已实现

✅ **VP 计算集成**：
- VP 计算已更新使用 `UnifiedDataService::fetch_ticks`
- `Message::ComputeVp` 和 `Message::VpComputed` 已实现
- VP 数据已能正确存储到 `ChartState`

✅ **消息机制**：
- `Message::FetchKLines(symbol, range, timeframe)` 已定义
- `Message::KLineDataFetched(result)` 已定义
- 数据获取逻辑已实现

### 1.2 待完成的工作

❌ **K 线数据存储**：
- `Message::KLineDataFetched` 的处理中只有 TODO 注释
- 获取的 K 线数据未存储到 `ChartState` 中
- 数据未传递给渲染层

❌ **可见范围变化检测**：
- 用户拖动/缩放图表时，未自动触发数据获取
- 需要实现可见范围变化检测机制

❌ **Symbol 切换**：
- 切换交易对时，未自动获取新数据
- 需要实现 symbol 切换时的数据获取逻辑

❌ **Timeframe 切换**：
- 切换周期时，未自动获取新数据
- 需要实现 timeframe 切换时的数据获取逻辑

---

## 二、设计目标

### 2.1 核心目标

1. **数据自动获取**：根据可见范围自动获取所需数据
2. **响应式更新**：用户交互（拖动、缩放、切换）时自动更新数据
3. **数据一致性**：确保 K 线数据和 VP 数据来自相同的数据源和时间范围
4. **性能优化**：避免重复请求，合理使用防抖机制

### 2.2 设计原则

1. **关注点分离**：渲染层不需要知道数据来源（实时/历史）
2. **统一接口**：通过 `UnifiedDataService` 获取数据
3. **异步处理**：数据获取不阻塞 UI 渲染
4. **错误处理**：优雅处理数据获取失败的情况

---

## 三、设计方案

### 3.1 数据流设计

```
用户交互（拖动/缩放/切换）
    ↓
检测可见范围变化 / Symbol 变化 / Timeframe 变化
    ↓
触发数据获取请求 (Message::FetchKLines)
    ↓
UnifiedDataService 自动选择数据源（实时/历史）
    ↓
数据返回 (Message::KLineDataFetched)
    ↓
存储到 ChartState 并更新渲染缓存
    ↓
触发重绘
```

### 3.2 关键组件设计

#### 3.2.1 K 线数据存储机制

**位置**：`flowsurface/src/main.rs` - `Message::KLineDataFetched` 处理

**设计**：
```rust
Message::KLineDataFetched(result) => {
    match result {
        Ok(klines) => {
            // 1. 找到对应的 Chart
            let dashboard = self.active_dashboard_mut();
            if let Some(pane) = dashboard.find_pane_by_symbol(&symbol) {
                if let Content::Kline { chart: Some(chart), .. } = &mut pane.content {
                    // 2. 转换数据格式：data::kline::KLine -> exchange::Kline
                    let exchange_klines: Vec<exchange::Kline> = klines
                        .iter()
                        .map(|k| convert_to_exchange_kline(k))
                        .collect();
                    
                    // 3. 存储到 ChartState
                    let req_id = uuid::Uuid::new_v4();
                    chart.insert_hist_klines(req_id, &exchange_klines);
                    
                    // 4. 触发 VP 计算（如果需要）
                    if let Some(action) = chart.check_vp_update_needed() {
                        // 处理 VP 计算请求
                    }
                }
            }
        }
        Err(e) => {
            // 错误处理：显示通知
        }
    }
}
```

**数据转换**：
- `data::kline::KLine` (微秒时间戳) → `exchange::Kline` (毫秒时间戳)
- 需要实现转换函数或 `From` trait

#### 3.2.2 可见范围变化检测

**位置**：`flowsurface/src/chart/kline.rs` - `KlineChart::invalidate`

**设计**：
```rust
impl KlineChart {
    fn invalidate(&mut self, now: Option<Instant>) -> Option<Action> {
        // ... 现有逻辑 ...
        
        // 检测可见范围变化
        if let Some(action) = self.check_data_update_needed() {
            return Some(action);
        }
        
        // ... 其他逻辑 ...
    }
    
    fn check_data_update_needed(&self) -> Option<Action> {
        // 1. 获取当前可见时间范围
        let visible_range = self.chart.state.visible_time_range_us()?;
        
        // 2. 检查是否需要新数据（与已有数据范围比较）
        if self.needs_data_for_range(&visible_range) {
            // 3. 获取 symbol 和 timeframe
            let symbol = self.chart.state.ticker_info.ticker.to_string();
            let timeframe = match self.chart.state.basis {
                Basis::Time(tf) => tf.to_string(),
                Basis::Tick(_) => return None, // Tick-based 不支持
            };
            
            // 4. 返回数据获取请求
            return Some(Action::RequestFetchKLines(symbol, visible_range, timeframe));
        }
        
        None
    }
    
    fn needs_data_for_range(&self, range: &TimeRange) -> bool {
        // 检查当前数据源是否覆盖了请求的范围
        // 如果数据不足，返回 true
        match &self.data_source {
            PlotData::TimeBased(timeseries) => {
                // 检查 timeseries 是否覆盖 range
                // 简化实现：如果数据为空或范围超出，返回 true
                timeseries.datapoints.is_empty() || 
                !self.has_data_for_range(range)
            }
            PlotData::TickBased(_) => false, // Tick-based 不支持
        }
    }
}
```

**防抖机制**：
- 使用 `last_data_request: Instant` 字段
- 限制请求频率（例如：每 500ms 最多一次）

#### 3.2.3 Symbol 切换处理

**位置**：`flowsurface/src/main.rs` - `Message::Dashboard` 处理

**设计**：
```rust
// 在 Dashboard Message 处理中检测 symbol 切换
Message::Dashboard(Some(pane_id), dashboard::Message::SymbolChanged(new_symbol)) => {
    // 1. 找到对应的 pane
    let dashboard = self.active_dashboard_mut();
    if let Some(pane) = dashboard.find_pane_mut(pane_id) {
        if let Content::Kline { chart: Some(chart), .. } = &mut pane.content {
            // 2. 更新 symbol
            chart.chart.state.ticker_info = new_ticker_info;
            
            // 3. 清空旧数据
            chart.data_source = PlotData::TimeBased(TimeSeries::new(...));
            chart.rebuild_render_cache();
            
            // 4. 获取可见范围并触发数据获取
            if let Some(visible_range) = chart.chart.state.visible_time_range_us() {
                let timeframe = match chart.chart.state.basis {
                    Basis::Time(tf) => tf.to_string(),
                    _ => "1m".to_string(),
                };
                return Task::done(Message::FetchKLines(
                    new_symbol,
                    visible_range,
                    timeframe,
                ));
            }
        }
    }
}
```

**注意**：需要检查现有的 symbol 切换机制，可能已经存在但未触发数据获取。

#### 3.2.4 Timeframe 切换处理

**位置**：`flowsurface/src/chart/kline.rs` - `KlineChart::set_basis`

**设计**：
```rust
pub fn set_basis(&mut self, new_basis: Basis) -> Option<Action> {
    // ... 现有逻辑 ...
    
    // 在切换后，触发数据获取
    if let Basis::Time(new_timeframe) = new_basis {
        // 1. 获取可见范围
        if let Some(visible_range) = self.chart.state.visible_time_range_us() {
            // 2. 获取 symbol
            let symbol = self.chart.state.ticker_info.ticker.to_string();
            let timeframe_str = new_timeframe.to_string();
            
            // 3. 返回数据获取请求
            return Some(Action::RequestFetchKLines(symbol, visible_range, timeframe_str));
        }
    }
    
    // ... 其他逻辑 ...
}
```

**注意**：`set_basis` 已经返回 `Option<Action>`，需要扩展 `Action` 枚举以支持 `RequestFetchKLines`。

### 3.3 Action 枚举扩展

**位置**：`flowsurface/src/chart.rs`

**设计**：
```rust
pub enum Action {
    ErrorOccurred(data::InternalError),
    RequestFetch(exchange::fetcher::FetchRequests),
    RequestVpComputation(String, data::TimeRange),
    RequestFetchKLines(String, data::TimeRange, String), // 新增：symbol, range, timeframe
}
```

**处理位置**：`flowsurface/src/screen/dashboard.rs` - `tick` 方法

**设计**：
```rust
chart::Action::RequestFetchKLines(symbol, range, timeframe) => {
    tasks.push(Task::done(Message::FetchKLines(symbol, range, timeframe)));
}
```

---

## 四、实现细节

### 4.1 数据转换函数

**位置**：`flowsurface/src/chart/kline.rs` 或新建工具模块

**实现**：
```rust
fn convert_to_exchange_kline(k: &data::kline::KLine) -> exchange::Kline {
    exchange::Kline {
        time: k.open_time_us / 1_000, // 微秒转毫秒
        open: Price::from_f64(k.open),
        high: Price::from_f64(k.high),
        low: Price::from_f64(k.low),
        close: Price::from_f64(k.close),
        volume: (k.volume as u64, 0), // 简化处理
    }
}
```

**或者实现 From trait**：
```rust
impl From<&data::kline::KLine> for exchange::Kline {
    fn from(k: &data::kline::KLine) -> Self {
        // ... 转换逻辑 ...
    }
}
```

### 4.2 可见范围比较逻辑

**位置**：`flowsurface/src/chart/kline.rs`

**实现**：
```rust
impl KlineChart {
    fn has_data_for_range(&self, range: &data::TimeRange) -> bool {
        match &self.data_source {
            PlotData::TimeBased(timeseries) => {
                if timeseries.datapoints.is_empty() {
                    return false;
                }
                
                // 获取数据的时间范围
                let data_start = timeseries.datapoints.keys().next()?;
                let data_end = timeseries.datapoints.keys().last()?;
                
                // 检查请求范围是否在数据范围内（考虑一些缓冲）
                let buffer = 60 * 1_000_000; // 1分钟缓冲（微秒）
                *data_start <= range.start_us.saturating_sub(buffer) &&
                *data_end >= range.end_us.saturating_add(buffer)
            }
            PlotData::TickBased(_) => false,
        }
    }
}
```

### 4.3 防抖机制

**位置**：`flowsurface/src/chart/kline.rs` - `KlineChart` 结构体

**实现**：
```rust
pub struct KlineChart {
    // ... 现有字段 ...
    last_data_request: Instant, // 新增字段
}

impl KlineChart {
    fn check_data_update_needed(&mut self) -> Option<Action> {
        // 防抖：限制请求频率
        if self.last_data_request.elapsed().as_millis() < 500 {
            return None;
        }
        
        // ... 检查逻辑 ...
        
        self.last_data_request = Instant::now();
        Some(action)
    }
}
```

---

## 五、错误处理

### 5.1 数据获取失败

**设计**：
- 显示错误通知（Toast）
- 记录错误日志
- 不阻塞 UI，允许用户重试

**实现**：
```rust
Message::KLineDataFetched(Err(e)) => {
    log::error!("Failed to fetch klines: {}", e);
    self.notifications.push(Toast::error(format!(
        "Failed to fetch K-line data: {}",
        e
    )));
}
```

### 5.2 数据为空

**设计**：
- 显示提示信息
- 不触发错误，允许用户继续操作

**实现**：
```rust
if klines.is_empty() {
    log::warn!("No K-line data received for range");
    // 可以选择显示提示或静默处理
}
```

---

## 六、性能考虑

### 6.1 请求频率限制

- **防抖时间**：500ms（可配置）
- **目的**：避免用户快速拖动时产生过多请求

### 6.2 数据范围检查

- **缓冲机制**：在数据范围检查时添加缓冲（例如：1分钟）
- **目的**：避免频繁的小范围请求

### 6.3 缓存利用

- **已有数据检查**：在请求前检查是否已有足够数据
- **目的**：避免重复请求相同数据

---

## 七、测试计划

### 7.1 单元测试

1. **数据转换测试**：
   - 测试 `data::kline::KLine` → `exchange::Kline` 转换
   - 验证时间戳、价格等字段正确转换

2. **范围检查测试**：
   - 测试 `has_data_for_range` 逻辑
   - 验证边界情况处理

### 7.2 集成测试

1. **数据获取流程测试**：
   - 模拟可见范围变化
   - 验证数据获取请求正确触发
   - 验证数据正确存储和渲染

2. **Symbol 切换测试**：
   - 切换交易对
   - 验证新数据正确获取和显示

3. **Timeframe 切换测试**：
   - 切换周期（1m → 5m → 1h）
   - 验证数据正确获取和显示

### 7.3 性能测试

1. **请求频率测试**：
   - 快速拖动图表
   - 验证防抖机制有效

2. **数据加载测试**：
   - 加载大量历史数据
   - 验证 UI 不卡顿

---

## 八、实施步骤

### Phase 1: 基础数据存储（优先级：高）

1. ✅ 实现数据转换函数
2. ✅ 完成 `Message::KLineDataFetched` 处理
3. ✅ 测试数据存储和渲染

### Phase 2: 可见范围检测（优先级：高）

1. ✅ 实现 `check_data_update_needed` 方法
2. ✅ 实现 `has_data_for_range` 方法
3. ✅ 扩展 `Action` 枚举
4. ✅ 在 `invalidate` 中集成检测逻辑
5. ✅ 测试可见范围变化触发

### Phase 3: Symbol 和 Timeframe 切换（优先级：中）

1. ✅ 实现 symbol 切换时的数据获取
2. ✅ 实现 timeframe 切换时的数据获取
3. ✅ 测试切换功能

### Phase 4: 优化和测试（优先级：中）

1. ✅ 添加防抖机制
2. ✅ 优化数据范围检查
3. ✅ 完善错误处理
4. ✅ 性能测试和优化

---

## 九、风险评估

### 9.1 技术风险

**风险 1**：数据转换可能丢失精度
- **缓解**：仔细测试转换逻辑，确保精度损失可接受

**风险 2**：频繁请求可能影响性能
- **缓解**：实现防抖机制，限制请求频率

**风险 3**：可见范围检测可能不准确
- **缓解**：添加缓冲机制，多次测试验证

### 9.2 集成风险

**风险 1**：与现有代码冲突
- **缓解**：仔细检查现有代码，确保兼容性

**风险 2**：数据格式不匹配
- **缓解**：充分测试数据转换，确保格式正确

---

## 十、后续优化方向

1. **预加载机制**：在用户拖动时预加载相邻范围的数据
2. **数据缓存**：在 ChartState 中缓存已获取的数据范围
3. **增量更新**：只获取缺失的数据，而不是重新获取整个范围
4. **后台加载**：在后台预加载常用时间范围的数据

---

## 十一、总结

本设计报告明确了渲染层和交互层需要完成的工作：

1. **核心功能**：K 线数据存储、可见范围检测、Symbol/Timeframe 切换
2. **设计原则**：关注点分离、统一接口、异步处理
3. **实施步骤**：分阶段实施，优先完成基础功能
4. **风险控制**：识别并缓解潜在风险

**下一步**：等待评审后开始实施。

