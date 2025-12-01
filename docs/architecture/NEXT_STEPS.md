# 统一数据管理架构 - 下一步实施指南

## 当前状态

### ✅ 已完成

1. **基础设施**
   - UnifiedDataManager: 全局数据管理器
   - ChartRegistry: 图表注册表
   - ChartDataManager Trait: 统一图表接口

2. **图表实现**
   - KlineChart: 完整实现
   - HeatmapChart: 完整实现
   - Ladder: 完整实现

3. **Dashboard 集成**
   - 初始化方法: `init_unified_data_manager()`, `on_startup()`
   - 配置开关: 环境变量 `FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER`
   - 注册方法: `register_chart()`, `unregister_chart()`

### ⏳ 待完成

1. **数据分发机制**
   - 在数据到达时通过 UnifiedDataManager 分发
   - 通知所有订阅者
   - 处理 RequestResult (Cached/Pending/NewRequest)

2. **自动注册集成**
   - 在 `pane.rs` 中图表创建时自动注册
   - 在图表销毁时自动注销

3. **数据请求路由**
   - 将现有的 `request_fetch` 逻辑迁移到 UnifiedDataManager
   - 处理缓存命中、待处理请求、新请求三种情况

## 实施步骤

### 步骤 1: 在图表创建时自动注册

在 `flowsurface/src/screen/dashboard/pane.rs` 中，当创建图表时添加注册逻辑：

```rust
// 在 Content::new_kline() 中，创建图表后：
let chart = KlineChart::new(...);

// 如果新架构已启用，注册图表
// 注意：需要 Dashboard 的引用，可能需要重构
if let Some(dashboard) = &mut dashboard {
    if let Some(id) = dashboard.register_chart(
        chart.clone(), // 注意：需要实现 Clone 或传递引用
        ChartType::Kline,
    ) {
        // 保存订阅者 ID 以便后续注销
    }
}
```

**挑战：**
- `Content` 枚举不直接访问 Dashboard
- 需要找到合适的地方传递 Dashboard 引用
- 或者通过 Message/Event 机制注册

**建议方案：**
1. 在 `Dashboard::update()` 中处理图表创建事件
2. 或者在 `Pane::set_content_and_streams()` 中注册

### 步骤 2: 实现数据分发机制

在 `flowsurface/src/screen/dashboard.rs` 的 `update()` 方法中，当数据到达时：

```rust
match message {
    Message::DataFetched { pane_id, stream, data } => {
        // 1. 处理原有逻辑
        // ...
        
        // 2. 如果新架构已启用，分发数据
        if let Some(data_manager) = &self.unified_data_manager {
            let key = DataKey::new(ticker_info, range, basis);
            let subscribers = data_manager.on_data_fetched(key, data.clone());
            
            // 3. 通知所有订阅者
            for subscriber_id in subscribers {
                // 通过 ChartRegistry 找到图表并通知
                // 或者通过消息机制通知
            }
        }
    }
    // ...
}
```

### 步骤 3: 迁移数据请求逻辑

在 `KlineChart::missing_data_task()` 中，添加 UnifiedDataManager 检查：

```rust
fn missing_data_task(&mut self) -> Option<Action> {
    // 1. 如果新架构已启用，先检查 UnifiedDataManager
    if let Some(data_manager) = &dashboard.unified_data_manager {
        let key = DataKey::new(ticker_info, range, basis);
        let requirements = self.data_requirements();
        
        match data_manager.request_data(key, self.subscriber_id, &requirements) {
            RequestResult::Cached(data) => {
                // 直接使用缓存数据
                self.insert_historical_data(data);
                return None; // 不需要发起新请求
            }
            RequestResult::Pending => {
                // 等待请求完成
                return None;
            }
            RequestResult::NewRequest(key) => {
                // 继续原有逻辑，发起新请求
            }
        }
    }
    
    // 2. 原有逻辑（如果 UnifiedDataManager 未启用或需要新请求）
    // ...
}
```

**挑战：**
- `missing_data_task` 需要访问 Dashboard 或 UnifiedDataManager
- 需要找到合适的方式传递引用

**建议方案：**
1. 通过 `ChartDataManager` trait 的方法参数传递
2. 或者在 Dashboard 层面统一处理数据请求

## 推荐实施顺序

### 阶段 A: 数据分发（优先级高）

1. 在 `Dashboard::update()` 中添加数据分发逻辑
2. 当数据到达时，通过 UnifiedDataManager 分发
3. 通知所有订阅者（通过消息机制）

### 阶段 B: 自动注册（优先级中）

1. 在图表创建时自动注册
2. 在图表销毁时自动注销
3. 保存订阅者 ID 以便后续使用

### 阶段 C: 数据请求路由（优先级低）

1. 逐步迁移现有请求逻辑
2. 处理缓存、待处理、新请求三种情况
3. 保持向后兼容

## 注意事项

1. **向后兼容**: 确保新架构未启用时，原有逻辑正常工作
2. **性能**: 数据分发可能影响性能，需要优化
3. **线程安全**: 确保所有操作都是线程安全的
4. **错误处理**: 添加适当的错误处理和日志

## 测试建议

1. **单元测试**: 测试 UnifiedDataManager 的缓存和去重
2. **集成测试**: 测试数据分发和通知机制
3. **性能测试**: 测试多图表场景下的性能
4. **兼容性测试**: 确保新架构未启用时功能正常

## 示例代码

### 在 Dashboard 中分发数据

```rust
impl Dashboard {
    pub fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::DataFetched { pane_id, stream, data } => {
                // 原有逻辑
                // ...
                
                // 新架构：分发数据
                self.distribute_data(&data);
            }
            // ...
        }
    }
    
    fn distribute_data(&mut self, data: &FetchedData) {
        if let Some(data_manager) = &self.unified_data_manager {
            // 构建 DataKey（需要从 data 中提取信息）
            // let key = DataKey::new(...);
            // let subscribers = data_manager.on_data_fetched(key, data.clone());
            // 通知订阅者
        }
    }
}
```

### 在 Pane 中注册图表

```rust
impl State {
    fn set_content(&mut self, content: Content, dashboard: &mut Dashboard) {
        match &content {
            Content::Kline { chart, .. } => {
                if let Some(chart) = chart {
                    dashboard.register_chart(chart, ChartType::Kline);
                }
            }
            // ...
        }
        self.content = content;
    }
}
```

## 总结

当前架构已经具备了完整的基础设施，下一步主要是：
1. 集成数据分发机制
2. 实现自动注册
3. 逐步迁移数据请求逻辑

建议按照优先级逐步实施，确保每一步都经过充分测试。


