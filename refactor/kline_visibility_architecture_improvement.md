# K线可见性检查架构改进方案

## 当前架构问题分析

### 1. 耦合问题
- **问题**：`YAxis` 需要知道 `KlineChart` 的存在，通过 `KlineChart::get_visible_klines_on_screen_static()` 调用
- **违反原则**：依赖倒置原则（DIP）- 高层模块（YAxis）不应该依赖低层模块（KlineChart）
- **影响**：如果将来有其他图表类型（如 `HeatmapChart`），它们也需要类似的逻辑，但无法复用

### 2. 职责不清
- **问题**：屏幕坐标计算逻辑属于坐标转换/可见性检查的通用功能，不应该绑定到 `KlineChart` 这个具体实现
- **影响**：代码复用性差，维护困难

### 3. 可扩展性差
- **问题**：如果将来需要支持其他类型的图表元素（如指标线、成交量柱等）的可见性检查，当前架构无法扩展
- **影响**：需要重复实现类似的逻辑

## 改进方案

### 方案A：独立的可见性工具模块（推荐）⭐

**架构设计**：
```
chart/
  ├── visibility.rs          # 新增：可见性检查工具模块
  ├── kline.rs
  ├── axes.rs
  └── ...
```

**实现**：
```rust
// chart/visibility.rs
pub mod visibility {
    use crate::chart::ChartState;
    use data::kline::KLine;
    
    /// 检查K线是否在屏幕上可见（基于屏幕坐标）
    pub fn get_visible_klines<'a>(
        chart_state: &ChartState,
        klines: &'a [KLine],
    ) -> Vec<&'a KLine> {
        // 屏幕坐标计算逻辑
        // ...
    }
    
    /// 检查是否有可见的K线
    pub fn has_visible_klines(
        chart_state: &ChartState,
        klines: &[KLine],
    ) -> bool {
        !get_visible_klines(chart_state, klines).is_empty()
    }
}
```

**优点**：
- ✅ **解耦**：`YAxis` 和 `KlineChart` 都不需要知道对方的存在
- ✅ **可复用**：其他图表类型（`HeatmapChart`、`ComparisonChart`）也可以使用
- ✅ **职责清晰**：可见性检查是独立的工具功能
- ✅ **易于测试**：可以独立测试可见性检查逻辑
- ✅ **易于扩展**：将来可以添加其他类型的可见性检查（如指标线、成交量柱等）

**缺点**：
- ⚠️ 需要创建一个新模块（但这是值得的）

### 方案B：作为 ChartState 的扩展方法

**架构设计**：
```rust
// chart.rs 或 chart/state.rs
impl ChartState {
    /// 获取可见的K线（基于屏幕坐标）
    pub fn get_visible_klines<'a>(
        &self,
        klines: &'a [data::kline::KLine],
    ) -> Vec<&'a data::kline::KLine> {
        // 屏幕坐标计算逻辑
        // ...
    }
}
```

**优点**：
- ✅ 逻辑上合理，因为可见性检查主要依赖 `ChartState` 的信息
- ✅ 不需要创建新模块

**缺点**：
- ⚠️ `ChartState` 需要知道 K-line 的结构，增加了耦合
- ⚠️ 如果将来需要检查其他类型的可见性（如指标线），`ChartState` 会变得臃肿

### 方案C：作为 Chart trait 的方法

**架构设计**：
```rust
pub trait Chart {
    // ... 现有方法 ...
    
    /// 获取可见的K线（基于屏幕坐标）
    fn get_visible_klines(&self) -> Vec<&data::kline::KLine>;
}
```

**优点**：
- ✅ 符合面向对象设计
- ✅ 类型安全

**缺点**：
- ⚠️ 不是所有 Chart 都有 K-line（如 `HeatmapChart`）
- ⚠️ 需要修改 trait，可能影响其他实现
- ⚠️ `YAxis` 仍然需要知道具体的 Chart 类型

## 推荐方案：方案A（独立工具模块）

### 实施步骤

1. **创建 `chart/visibility.rs` 模块**
   - 将屏幕坐标计算逻辑从 `KlineChart` 提取出来
   - 提供通用的 `get_visible_klines()` 和 `has_visible_klines()` 函数

2. **更新 `KlineChart`**
   - 移除 `get_visible_klines_on_screen_static()` 方法
   - 使用 `visibility::get_visible_klines()` 和 `visibility::has_visible_klines()`

3. **更新 `YAxis`**
   - 移除对 `KlineChart` 的依赖
   - 使用 `visibility::get_visible_klines()`

4. **更新其他使用可见性检查的地方**
   - `chart_data()` 方法
   - `update_y_axis_range_cache()` 方法
   - `invalidate()` 方法
   - `update_debug_visible_range()` 方法

### 架构优势

1. **单一职责原则（SRP）**：可见性检查是独立的工具功能
2. **依赖倒置原则（DIP）**：高层模块（YAxis）依赖抽象（visibility模块），而不是具体实现（KlineChart）
3. **开闭原则（OCP）**：对扩展开放（可以添加其他类型的可见性检查），对修改封闭（不需要修改现有代码）
4. **可测试性**：可以独立测试可见性检查逻辑
5. **可维护性**：代码集中，易于维护和修改

## 未来扩展

在 `visibility.rs` 中可以进一步扩展：

```rust
pub mod visibility {
    // K-line 可见性检查
    pub fn get_visible_klines(...) -> Vec<&KLine> { ... }
    
    // 将来可以添加：
    // - 指标线可见性检查
    // - 成交量柱可见性检查
    // - 通用元素可见性检查（基于时间范围）
    // - 等等
}
```

## 总结

**推荐采用方案A**，因为它：
- 最符合SOLID原则
- 提供了最好的解耦和可扩展性
- 代码结构最清晰
- 易于测试和维护

