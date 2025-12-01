# 统一架构迁移计划（保证向后兼容）

## 迁移原则

1. **渐进式迁移**：逐步迁移，每个步骤都可以独立验证
2. **向后兼容**：新旧代码可以共存
3. **功能验证**：每个阶段都要验证功能正常
4. **可回滚**：如果出现问题，可以回滚到上一步

## 迁移阶段

### 阶段 0：准备阶段（不修改现有代码）

**目标**：创建新架构的基础设施，但不影响现有代码

**任务**：
1. ✅ 创建 `UnifiedDataManager` 结构（独立模块）
2. ✅ 创建 `ChartDataManager` trait 定义
3. ✅ 创建 `ChartRegistry` 结构
4. ✅ 在 Dashboard 中添加新组件（可选使用）

**验证**：
- 编译通过
- 现有功能完全正常
- 新代码未被调用

**代码位置**：
```
flowsurface/src/screen/dashboard/
  ├─ data_manager.rs (新建，不导出)
  └─ chart_registry.rs (新建，不导出)
```

### 阶段 1：并行实现（新旧共存）

**目标**：为新架构实现接口，但保持现有代码不变

**任务**：
1. 为 `KlineChart` 实现 `ChartDataManager` trait（可选实现）
2. 为 `HeatmapChart` 实现 `ChartDataManager` trait（可选实现）
3. 为 `Ladder` 实现 `ChartDataManager` trait（可选实现）
4. 在 Dashboard 中同时维护新旧两套系统

**关键设计**：
```rust
impl KlineChart {
    // 保留所有现有方法不变
    pub fn missing_data_task(&mut self) -> Option<Action> {
        // 现有实现保持不变
    }
    
    // 新增：实现 ChartDataManager（可选）
    // 但默认不启用
}

// 在 Dashboard 中
impl Dashboard {
    // 保留现有方法
    pub fn handle_old_way(&mut self) { /* 现有实现 */ }
    
    // 新增方法（可选使用）
    pub fn handle_new_way(&mut self) { /* 新实现 */ }
}
```

**验证**：
- 现有功能完全正常
- 新接口可以编译
- 可以通过配置开关选择使用新旧方式

### 阶段 2：逐步切换（一个图表一个图表）

**目标**：逐个图表迁移到新架构

**迁移顺序**：
1. **HeatmapChart**（最简单，只有实时数据）
2. **Ladder**（类似 Heatmap）
3. **KlineChart**（最复杂，需要完整测试）

**每个图表的迁移步骤**：

#### 步骤 2.1：迁移 HeatmapChart

**修改**：
```rust
impl HeatmapChart {
    // 1. 添加数据管理器引用（可选）
    data_manager: Option<Arc<UnifiedDataManager>>,
    
    // 2. 实现 ChartDataManager（可选）
    // 3. 保持现有方法不变
    pub fn insert_datapoint(&mut self, ...) {
        // 现有实现保持不变
        // 可选：同时更新新系统
    }
}
```

**验证清单**：
- [ ] Heatmap 图表正常显示
- [ ] 实时数据正常更新
- [ ] 订单簿深度正常显示
- [ ] 所有 Heatmap 功能正常

**回滚方案**：
- 如果出现问题，移除 `data_manager` 字段
- 恢复原有实现

#### 步骤 2.2：迁移 Ladder

**类似 HeatmapChart 的迁移步骤**

**验证清单**：
- [ ] Ladder 正常显示
- [ ] 订单簿正常更新
- [ ] 交易数据正常显示
- [ ] 所有 Ladder 功能正常

#### 步骤 2.3：迁移 KlineChart

**这是最复杂的迁移，需要特别小心**

**分步骤迁移**：

**2.3.1：添加新接口（不启用）**
```rust
impl KlineChart {
    // 保留所有现有字段和方法
    request_handler: RequestHandler,  // 保留
    raw_trades: Vec<Trade>,           // 保留
    fetching_trades: (bool, Option<Handle>),  // 保留
    
    // 新增（可选）
    data_manager: Option<Arc<UnifiedDataManager>>,
}

impl ChartDataManager for KlineChart {
    // 实现新接口，但默认不启用
}
```

**2.3.2：双写模式（同时更新新旧系统）**
```rust
impl KlineChart {
    pub fn insert_trades_buffer(&mut self, trades_buffer: &[Trade]) {
        // 1. 现有逻辑（保持不变）
        self.raw_trades.extend_from_slice(trades_buffer);
        match self.data_source {
            PlotData::TimeBased(ref mut timeseries) => {
                // 现有逻辑
            }
            // ...
        }
        
        // 2. 新系统（可选，通过配置开关控制）
        if let Some(dm) = &self.data_manager {
            // 同时更新新系统
        }
    }
}
```

**2.3.3：切换到新系统（逐步）**
- 先切换数据请求逻辑
- 再切换数据存储逻辑
- 最后移除旧代码

**验证清单**：
- [ ] Footprint 图表正常显示
- [ ] Candles 图表正常显示
- [ ] HVN 计算正常
- [ ] NPoC 显示正常
- [ ] Imbalance 显示正常
- [ ] 历史数据下载正常
- [ ] 实时数据更新正常
- [ ] 所有指标正常
- [ ] 所有交互功能正常

### 阶段 3：完全切换

**目标**：所有图表都使用新架构

**任务**：
1. 移除旧的 `RequestHandler`（如果不再需要）
2. 统一使用 `UnifiedDataManager`
3. 清理重复代码

**验证**：
- 所有图表功能正常
- 性能没有下降
- 内存使用优化

### 阶段 4：优化

**目标**：优化和清理

**任务**：
1. 优化缓存策略
2. 优化请求合并
3. 性能调优
4. 代码清理

## 测试策略

### 1. 单元测试

为每个新组件编写单元测试：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_unified_data_manager_request_deduplication() {
        // 测试请求去重
    }
    
    #[test]
    fn test_data_sharing_between_charts() {
        // 测试数据共享
    }
}
```

### 2. 集成测试

测试图表之间的交互：
```rust
#[test]
fn test_footprint_and_candles_share_data() {
    // 创建两个图表
    // 请求相同数据
    // 验证数据共享
}
```

### 3. 功能回归测试

每个迁移步骤后，验证所有功能：

**KlineChart 功能清单**：
- [ ] 图表渲染
- [ ] 数据加载
- [ ] 实时更新
- [ ] 历史数据下载
- [ ] HVN 计算和显示
- [ ] NPoC 显示
- [ ] Imbalance 显示
- [ ] 指标计算
- [ ] 交互功能（缩放、平移等）
- [ ] 设置修改

**HeatmapChart 功能清单**：
- [ ] 图表渲染
- [ ] 实时数据更新
- [ ] 订单簿深度显示
- [ ] 交易数据聚合
- [ ] 所有 Heatmap 功能

**Ladder 功能清单**：
- [ ] 订单簿显示
- [ ] 交易数据显示
- [ ] 所有 Ladder 功能

## 配置开关

使用配置开关控制新旧系统：

```rust
// 在配置中
pub struct Config {
    pub use_unified_data_manager: bool,  // 默认 false
    pub migrate_kline_chart: bool,      // 默认 false
    pub migrate_heatmap_chart: bool,    // 默认 false
    pub migrate_ladder: bool,            // 默认 false
}

// 在代码中
if config.use_unified_data_manager && config.migrate_kline_chart {
    // 使用新系统
} else {
    // 使用旧系统
}
```

## 回滚方案

每个阶段都有明确的回滚点：

1. **阶段 0 → 回滚**：删除新文件即可
2. **阶段 1 → 回滚**：移除 trait 实现
3. **阶段 2 → 回滚**：恢复原有方法实现
4. **阶段 3 → 回滚**：恢复旧代码，移除新代码

## 监控和日志

添加详细的日志来监控迁移过程：

```rust
log::debug!("UnifiedDataManager: Request for {:?} from {:?}", key, subscriber);
log::debug!("UnifiedDataManager: Cache hit for {:?}", key);
log::debug!("UnifiedDataManager: New request for {:?}", key);
log::debug!("UnifiedDataManager: Data fetched for {:?}, notifying {} subscribers", key, count);
```

## 总结

这个迁移计划：

✅ **渐进式**：逐步迁移，每步可验证  
✅ **向后兼容**：新旧代码可以共存  
✅ **可回滚**：每个阶段都可以回滚  
✅ **功能保证**：每个步骤都有验证清单  
✅ **风险可控**：一个图表一个图表迁移  

这样可以确保在迁移过程中，现有功能始终保持正常工作。


