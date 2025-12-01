# 统一数据管理架构 - 使用指南

## 概述

统一数据管理架构提供了全局的数据获取、缓存和共享机制。默认情况下，该架构**不启用**，不会影响现有功能。

## 启用新架构

### 方法 1: 环境变量

设置环境变量来启用：

```bash
export FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true
# 或
export FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=1
```

### 方法 2: 代码中启用

在 Dashboard 初始化后调用：

```rust
dashboard.on_startup(); // 会自动检查环境变量并初始化
// 或
dashboard.init_unified_data_manager(); // 直接初始化
```

## 架构组件

### UnifiedDataManager

全局数据管理器，负责：
- 数据缓存
- 请求去重
- 订阅者管理

### ChartRegistry

图表注册表，负责：
- 注册/注销图表
- 按类型索引图表
- 生命周期管理

### ChartDataManager Trait

统一的图表接口，每个图表需要实现：
- `data_requirements()`: 声明数据需求
- `subscriber_id()`: 返回唯一订阅者 ID

## 已实现的图表

### KlineChart ✅

- 支持 Footprint 和 Candles
- 根据 chart kind 和 enabled studies 决定数据需求
- 例如：Candles 只有在启用 HVN 时才需要 trades

### HeatmapChart ✅

- 需要 trades 数据
- 支持历史数据
- 仅支持 Time-based aggregation

### Ladder ✅

- 需要 depth 和 trades 数据
- 仅实时数据（不支持历史数据）

## 使用示例

### 注册图表

```rust
if dashboard.is_unified_data_manager_enabled() {
    if let Some(registry) = &mut dashboard.chart_registry {
        let chart_id = registry.register_chart(
            kline_chart,
            ChartType::Kline,
        );
    }
}
```

### 请求数据

```rust
if let Some(data_manager) = &dashboard.unified_data_manager {
    let key = DataKey::new(ticker_info, range, basis);
    let requirements = chart.data_requirements();
    
    match data_manager.request_data(key, chart.subscriber_id(), &requirements) {
        RequestResult::Cached(data) => {
            // 直接使用缓存数据
            chart.insert_historical_data(data);
        }
        RequestResult::Pending => {
            // 等待请求完成
        }
        RequestResult::NewRequest(key) => {
            // 发起新请求
            // 请求完成后调用 data_manager.on_data_fetched(key, data)
        }
    }
}
```

## 迁移计划

### 阶段 1: 基础设施 ✅
- [x] UnifiedDataManager
- [x] ChartRegistry
- [x] ChartDataManager Trait

### 阶段 2: 图表实现 ✅
- [x] KlineChart
- [x] HeatmapChart
- [x] Ladder

### 阶段 3: Dashboard 集成 ⏳
- [x] 初始化方法
- [ ] 自动注册图表
- [ ] 数据请求路由
- [ ] 数据分发

### 阶段 4: 测试和优化 ⏳
- [ ] 功能测试
- [ ] 性能优化
- [ ] 向后兼容性验证

## 注意事项

1. **向后兼容**: 新架构默认不启用，现有功能不受影响
2. **渐进式迁移**: 可以逐个图表迁移，不需要一次性完成
3. **性能**: 启用后会增加内存使用（缓存），但会减少重复请求
4. **线程安全**: 所有组件都是线程安全的（使用 Arc<RwLock<>>）

## 故障排查

### 新架构未启用

检查环境变量：
```bash
echo $FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER
```

或在代码中检查：
```rust
if dashboard.is_unified_data_manager_enabled() {
    // 已启用
} else {
    // 未启用
}
```

### 图表未注册

确保在创建图表后调用注册：
```rust
let chart = KlineChart::new(...);
if let Some(registry) = &mut dashboard.chart_registry {
    registry.register_chart(chart, ChartType::Kline);
}
```

## 未来扩展

- [ ] 支持更多图表类型
- [ ] 缓存清理策略优化
- [ ] 性能监控和指标
- [ ] 配置持久化


