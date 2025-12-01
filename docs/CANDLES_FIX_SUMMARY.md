# Candles 图启动时只显示一根 K 线 - 修复总结

## 问题描述

- **Footprint 图**：启动后可以正常加载多个 K 线（673 根）
- **Candles 图**：启动后只显示一根 K 线，需要切换 timeframe 后才全部显示

## 根本原因

### 1. `visible_timerange` 计算错误

从日志可以看到，Candles 图初始加载时：
- `region.x = -787`（负数）
- `translation.x = 377.5`
- `latest_x = 1764583200000`（只有 1 根 K 线时）
- `cell_width = 4.0`

当 `region.x` 为负数时，`x_to_interval(-787)` 的计算：
```rust
if x <= 0.0 {
    let diff = (-x / self.cell_width * interval as f32) as u64;
    self.latest_x.saturating_sub(diff)
}
```

对于 M30（interval = 1800000 毫秒）：
- `diff = (787 / 4.0 * 1800000)` ≈ `354150000` 毫秒（约 4.1 天）
- `latest_x.saturating_sub(354150000)` 如果 `latest_x` 很小，可能返回 0

这导致 `visible_earliest = 0`，`visible_latest = 15300000`（约 4.25 小时），这是一个无效的时间范围。

### 2. 为什么 Footprint 没问题

- Footprint 初始时 `bounds.size = (0, 0)`
- `visible_region` 返回 `width = 0`
- `visible_timerange` 返回 `None`
- 使用默认范围，这是合理的

### 3. 为什么 Candles 有问题

- Candles 初始时 `bounds.size = (819, 657.5)`（已经有 bounds）
- `x_translation` 的计算基于 `chart.bounds.width`，但初始时可能还没有正确设置
- 导致 `translation.x` 计算错误，`region.x` 为负数且很大
- `x_to_interval(-787)` 返回了 0

## 修复方案

### 1. 在 `visible_timerange` 中添加验证逻辑

```rust
// 检查计算结果是否合理
let one_day_ms = 24 * 60 * 60 * 1000;
if (earliest == 0 && latest < one_day_ms) || earliest > latest {
    log::warn!(
        "[{}] visible_timerange: Invalid result detected, earliest={}, latest={}, returning None",
        chart_kind,
        earliest,
        latest
    );
    return None;
}
```

### 2. 在 `missing_data_task` 中添加验证和默认范围

```rust
// 检查返回的时间范围是否合理
let one_day_ms = 24 * 60 * 60 * 1000;
if range.0 == 0 && range.1 < one_day_ms {
    // 使用默认范围
    ...
}
```

### 3. 添加详细的调试日志

- 记录 `translation.x`、`latest_x`、`cell_width` 等中间值
- 记录 `earliest_raw`、`latest_raw` 等计算结果
- 便于后续调试和问题定位

## 修复效果

从最新日志可以看到：
- Candles 图现在有 **673 根 K 线**（之前只有 1 根）
- `visible_timerange` 返回了合理的时间范围：
  - `earliest_raw=1764229049984`（约 2025-11-27 14:30）
  - `latest_raw=1764597600000`（约 2025-11-27 23:00）
- Footprint 图也有 673 根 K 线
- 两者都在正常工作，请求后续数据

## 关键改进

1. **验证逻辑**：在 `visible_timerange` 中直接检测无效结果并返回 `None`
2. **默认范围**：当 `visible_timerange` 返回 `None` 或无效值时，使用基于数据或当前时间的合理默认范围
3. **详细日志**：添加了完整的调试信息，便于问题定位

## 后续优化建议

1. **根本修复**：考虑修复 `x_to_interval` 对负数的处理，或者确保 `translation.x` 在初始时正确设置
2. **清理日志**：问题解决后，可以考虑将部分调试日志改为 `trace` 级别或移除
3. **统一初始化**：确保 Footprint 和 Candles 图的初始化逻辑一致

