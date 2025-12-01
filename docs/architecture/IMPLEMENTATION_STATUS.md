# 统一数据管理架构 - 实施状态

## 概述

本文档记录统一数据管理架构的实施进度和当前状态。

## 已完成的工作

### 阶段 0: 基础设施 ✅

1. **UnifiedDataManager** (`flowsurface/src/screen/dashboard/unified_data_manager.rs`)
   - ✅ 数据键（DataKey）定义
   - ✅ 数据需求（DataRequirements）定义
   - ✅ 全局请求去重（GlobalRequestDeduplicator）
   - ✅ 数据缓存机制
   - ✅ 订阅者管理

2. **ChartRegistry** (`flowsurface/src/screen/dashboard/chart_registry.rs`)
   - ✅ 图表注册表结构
   - ✅ 按类型索引图表
   - ✅ 生命周期管理

3. **ChartDataManager Trait** (`flowsurface/src/screen/dashboard/chart_traits.rs`)
   - ✅ 统一的图表接口定义
   - ✅ 数据需求声明
   - ✅ 实时数据插入接口
   - ✅ 历史数据插入接口

4. **Dashboard 集成**
   - ✅ 在 Dashboard 中添加可选字段（默认不启用）
   - ✅ 不影响现有功能

### 阶段 1: KlineChart 实现 ✅

1. **KlineChart 修改**
   - ✅ 添加 `subscriber_id` 字段
   - ✅ 在 `new()` 方法中初始化唯一 ID

2. **ChartDataManager 实现**
   - ✅ 实现 `data_requirements()` 方法
     - 根据 chart kind（Footprint/Candles）决定数据需求
     - 根据 enabled studies（如 HVN）决定是否需要 trades
   - ✅ 实现 `subscriber_id()` 方法

## 待完成的工作

### 阶段 2: 其他图表实现

1. **HeatmapChart** (`flowsurface/src/chart/heatmap.rs`)
   - ⏳ 添加 `subscriber_id` 字段
   - ⏳ 实现 `ChartDataManager` trait
   - ⏳ 定义数据需求（仅需要 trades，支持历史数据）

2. **Ladder** (`flowsurface/src/screen/dashboard/panel/ladder.rs`)
   - ⏳ 添加 `subscriber_id` 字段
   - ⏳ 实现 `ChartDataManager` trait
   - ⏳ 定义数据需求（需要 depth 和 trades，仅实时数据）

3. **TimeAndSales** (如果适用)
   - ⏳ 评估是否需要实现

### 阶段 3: Dashboard 集成

1. **初始化 UnifiedDataManager**
   - ⏳ 在 Dashboard::new() 或 Dashboard::from_config() 中初始化
   - ⏳ 添加配置开关（环境变量或配置文件）

2. **注册图表**
   - ⏳ 在创建图表时注册到 ChartRegistry
   - ⏳ 在销毁图表时注销

3. **数据请求路由**
   - ⏳ 修改现有的数据请求逻辑，通过 UnifiedDataManager
   - ⏳ 处理请求结果（Cached/Pending/NewRequest）

4. **数据分发**
   - ⏳ 在数据到达时，通过 UnifiedDataManager 分发
   - ⏳ 通知所有订阅者

### 阶段 4: 测试和优化

1. **功能测试**
   - ⏳ 测试请求去重
   - ⏳ 测试数据缓存
   - ⏳ 测试多图表共享数据

2. **性能优化**
   - ⏳ 缓存清理策略优化
   - ⏳ 内存使用监控

3. **向后兼容**
   - ⏳ 确保现有功能不受影响
   - ⏳ 逐步迁移现有图表

## 文件结构

```
flowsurface/
├── src/
│   ├── screen/
│   │   └── dashboard/
│   │       ├── unified_data_manager.rs  ✅
│   │       ├── chart_registry.rs         ✅
│   │       └── chart_traits.rs           ✅
│   └── chart/
│       ├── kline.rs                      ✅ (已添加 subscriber_id)
│       └── kline_data_manager.rs         ✅
└── docs/
    └── architecture/
        ├── ARCHITECTURE_SUMMARY.md        ✅
        └── IMPLEMENTATION_STATUS.md       ✅ (本文档)
```

## 使用说明

### 当前状态

新架构的基础设施已经就绪，但**默认不启用**，不会影响现有功能。

### 启用新架构（待实现）

1. 在 Dashboard 中初始化 UnifiedDataManager：
```rust
let data_manager = Arc::new(UnifiedDataManager::new(1000));
let chart_registry = ChartRegistry::new(data_manager.clone());
```

2. 注册图表：
```rust
let chart_id = chart_registry.register_chart(kline_chart, ChartType::Kline);
```

3. 通过 UnifiedDataManager 请求数据：
```rust
match data_manager.request_data(key, subscriber_id, &requirements) {
    RequestResult::Cached(data) => {
        // 直接使用缓存数据
    }
    RequestResult::Pending => {
        // 等待请求完成
    }
    RequestResult::NewRequest(key) => {
        // 发起新请求
    }
}
```

## 注意事项

1. **向后兼容**：所有新代码都标记为 `#[allow(dead_code)]`，确保不影响现有功能
2. **渐进式迁移**：可以逐个图表迁移，不需要一次性完成
3. **测试**：每个阶段完成后都应该进行充分测试

## 下一步

1. 为 HeatmapChart 和 Ladder 实现 ChartDataManager trait
2. 在 Dashboard 中添加初始化逻辑
3. 逐步迁移数据请求逻辑

