# 统一图表架构设计总结

## 核心设计原则

### 1. 分层架构

```
┌─────────────────────────────────────────┐
│  Dashboard Layer (协调层)               │
│  - UnifiedDataManager                  │
│  - ChartRegistry                        │
└─────────────────────────────────────────┘
              │
              ▼
┌─────────────────────────────────────────┐
│  Trait Layer (接口层)                   │
│  - Chart                                │
│  - ChartDataManager                     │
└─────────────────────────────────────────┘
              │
              ▼
┌─────────────────────────────────────────┐
│  Implementation Layer (实现层)           │
│  - KlineChart, HeatmapChart, Ladder...  │
└─────────────────────────────────────────┘
```

### 2. 统一接口设计

所有图表类型实现统一的 `ChartDataManager` trait：

```rust
pub trait ChartDataManager {
    // 1. 声明数据需求（让系统知道需要什么数据）
    fn data_requirements(&self) -> DataRequirements;
    
    // 2. 请求数据（可选，实时图表可能不需要）
    fn request_data(&mut self, range: FetchRange) -> Option<Action>;
    
    // 3. 插入实时数据（必须实现）
    fn insert_realtime_data(&mut self, data: &RealtimeData);
    
    // 4. 插入历史数据（可选）
    fn insert_historical_data(&mut self, data: Arc<FetchedData>);
}
```

### 3. 数据需求声明式设计

每个图表通过 `DataRequirements` 声明自己的数据需求：

```rust
// KlineChart (Footprint/Candles)
DataRequirements {
    needs_klines: true,
    needs_trades: true,  // 根据是否启用 HVN
    supports_historical: true,
    supports_tick_basis: true,
}

// HeatmapChart
DataRequirements {
    needs_trades: true,
    needs_depth: true,
    supports_historical: false,  // 不支持历史数据
}

// Ladder
DataRequirements {
    needs_trades: true,
    needs_depth: true,
    supports_historical: false,
}
```

## 关键组件

### 1. UnifiedDataManager（全局数据管理器）

**职责**：
- 统一管理所有数据请求
- 全局请求去重
- 数据缓存管理
- 订阅者管理

**优势**：
- 相同数据只下载一次
- 相同数据只存储一份（通过 Arc 共享）
- 自动处理订阅和通知

### 2. ChartRegistry（图表注册表）

**职责**：
- 管理所有图表实例
- 生命周期管理
- 按类型索引

**优势**：
- 统一管理所有图表
- 便于查找和操作
- 支持批量操作

### 3. GlobalRequestDeduplicator（全局请求去重器）

**职责**：
- 跨实例的请求去重
- 订阅者管理
- 请求状态跟踪

**优势**：
- 避免重复下载
- 多个图表可以等待同一请求

## 数据流

### 场景 1：Footprint 和 Candles 请求相同数据

```
1. Footprint 请求 Trades(1000-2000)
   └─> UnifiedDataManager::request_data()
       ├─> 检查缓存 → 未命中
       ├─> 检查 pending → 无
       └─> 创建新请求，注册到 deduplicator

2. Candles 请求相同数据
   └─> UnifiedDataManager::request_data()
       ├─> 检查缓存 → 未命中
       ├─> 检查 pending → 发现 Footprint 的请求 ✅
       └─> 添加到订阅列表，返回 Pending

3. 数据到达
   └─> UnifiedDataManager::on_data_fetched()
       ├─> 更新缓存
       ├─> 获取所有订阅者 [Footprint, Candles]
       └─> 通知两个图表
           ├─> Footprint 收到数据
           └─> Candles 收到数据（共享同一份 Arc）
```

### 场景 2：Heatmap 和 Ladder 接收实时数据

```
实时数据流到达
  └─> Dashboard::handle_realtime_data()
      └─> ChartRegistry::charts 遍历
          ├─> Heatmap: 检查 requirements → 需要 depth+trades ✅
          │   └─> insert_realtime_data()
          └─> Ladder: 检查 requirements → 需要 depth+trades ✅
              └─> insert_realtime_data()
```

## 扩展新图表类型

### 步骤 1：定义图表结构

```rust
pub struct NewChartType {
    id: SubscriberId,
    data_manager: Arc<UnifiedDataManager>,
    // ... 其他字段
}
```

### 步骤 2：实现 Chart trait

```rust
impl Chart for NewChartType {
    // 实现必需方法
}
```

### 步骤 3：实现 ChartDataManager trait

```rust
impl ChartDataManager for NewChartType {
    fn data_requirements(&self) -> DataRequirements {
        // 声明数据需求
    }
    
    fn insert_realtime_data(&mut self, data: &RealtimeData) {
        // 处理实时数据
    }
    
    // 可选：实现历史数据支持
    fn request_data(&mut self, range: FetchRange) -> Option<Action> {
        // 如果需要历史数据
    }
}
```

### 步骤 4：注册到 Dashboard

```rust
let chart = NewChartType::new(...);
let id = dashboard.chart_registry.register_chart(chart, ChartType::NewType);
```

**完成！** 新图表类型自动获得：
- ✅ 数据请求去重
- ✅ 数据缓存共享
- ✅ 实时数据分发
- ✅ 历史数据支持（如果实现）

## 架构优势

### 1. 可扩展性

- **添加新图表类型**：只需实现 trait，无需修改现有代码
- **添加新数据源**：在 UnifiedDataManager 中扩展
- **添加新功能**：在 trait 中添加新方法

### 2. 数据效率

- **零拷贝共享**：使用 Arc 共享数据
- **请求去重**：相同请求只执行一次
- **智能缓存**：自动管理缓存生命周期

### 3. 代码质量

- **类型安全**：Rust 类型系统保证
- **职责分离**：数据管理、渲染、交互分离
- **易于测试**：trait 可以 mock

### 4. 维护性

- **统一接口**：所有图表遵循相同模式
- **集中管理**：数据管理逻辑集中
- **易于调试**：统一的日志和错误处理

## 迁移策略

### 阶段 1：基础设施（不破坏现有代码）

1. 创建 `UnifiedDataManager` 和 `ChartRegistry`
2. 定义 `ChartDataManager` trait
3. 在 Dashboard 中集成，但保持现有代码不变

### 阶段 2：逐步迁移

1. 为 `KlineChart` 实现 `ChartDataManager`
2. 为 `HeatmapChart` 实现 `ChartDataManager`
3. 为 `Ladder` 实现 `ChartDataManager`
4. 每个图表迁移后测试

### 阶段 3：统一使用

1. 所有图表通过 `ChartRegistry` 管理
2. 所有数据请求通过 `UnifiedDataManager`
3. 移除旧的独立 `RequestHandler`

### 阶段 4：优化

1. 优化缓存策略
2. 优化请求合并
3. 性能调优

## 总结

这个统一架构设计：

✅ **统一接口**：所有图表实现相同的 trait  
✅ **可扩展**：新图表类型易于添加  
✅ **数据共享**：最大化数据复用  
✅ **类型安全**：Rust 类型系统保证  
✅ **向后兼容**：可以逐步迁移  
✅ **易于维护**：代码结构清晰  

这是一个**从顶层设计、保证可扩展性**的架构方案。

