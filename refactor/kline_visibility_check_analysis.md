# K线可见性检查方法一致性分析

## 问题总结

程序中存在**两种不同的方法**来检查屏幕可见范围内的K线数据：

### 1. 屏幕坐标检查（准确）
- **位置**：
  - `has_visible_klines_on_screen()` (line 1240)
  - `invalidate()` 方法中的检查 (line 773-790)
  - `update_debug_visible_range()` 方法中的检查 (line 931-940)
- **逻辑**：计算K线的屏幕坐标，检查是否与屏幕边界重叠
- **优点**：准确反映屏幕上实际可见的K线

### 2. 时间范围检查（不准确）
- **位置**：
  - `calculate_price_range()` 方法 (line 1012-1014)
  - `axes.rs` 中的Y轴标签生成 (line 322-324)
  - `invalidate()` 和 `update_debug_visible_range()` 中的 fallback (line 807-809, 954-956)
- **逻辑**：使用时间范围过滤：`k.open_time_us >= start_ts_us && k.open_time_us <= end_ts_us`
- **缺点**：时间范围内的K线不一定在屏幕上可见（可能因为缩放、平移等原因）

## 不一致的影响

1. **Y轴价格范围计算不准确**：`calculate_price_range()` 使用时间范围，可能包含不在屏幕上的K线
2. **Y轴标签位置不准确**：`axes.rs` 使用时间范围，可能导致标签显示错误
3. **代码重复**：屏幕坐标计算逻辑在多个地方重复

## 建议的解决方案

### 方案A：统一使用屏幕坐标检查（推荐）

1. **创建统一的辅助方法**：
   ```rust
   /// 获取屏幕上可见的K线（使用屏幕坐标检查）
   fn get_visible_klines_on_screen(&self) -> Vec<&data::kline::KLine>
   ```

2. **更新所有使用时间范围检查的地方**：
   - `calculate_price_range()` → 使用屏幕坐标检查
   - `axes.rs` 中的Y轴标签生成 → 使用屏幕坐标检查
   - 移除或简化 fallback 逻辑

3. **代码复用**：
   - 所有屏幕坐标计算逻辑统一到一个方法中
   - 其他方法调用这个统一方法

### 方案B：保留 fallback 但优先使用屏幕坐标

- 主要逻辑使用屏幕坐标检查
- 当屏幕坐标检查失败时，fallback 到时间范围检查
- 适用于边界情况（如缩放为0等）

## 需要修改的文件

1. `flowsurface/src/chart/kline.rs`
   - `calculate_price_range()` - 改为使用屏幕坐标检查
   - 创建统一的 `get_visible_klines_on_screen()` 方法
   - 简化 `invalidate()` 和 `update_debug_visible_range()` 中的重复代码

2. `flowsurface/src/chart/axes.rs`
   - Y轴标签生成逻辑 - 改为使用屏幕坐标检查

## 实施优先级

**高优先级**：
- `calculate_price_range()` - 影响Y轴价格范围准确性
- `axes.rs` 中的Y轴标签生成 - 影响标签显示准确性

**中优先级**：
- 代码重构，统一屏幕坐标计算逻辑

**低优先级**：
- 优化 fallback 逻辑

