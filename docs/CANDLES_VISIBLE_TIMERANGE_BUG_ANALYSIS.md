# Candles 图 visible_timerange 错误分析

## 问题描述

从日志可以看到：
```
[Candles] visible_timerange: region=(-787, 819), bounds.size=Size { width: 819.0, height: 657.5 }, result=(0, 15300000)
```

Candles 图的 `visible_timerange` 返回了错误的时间范围 `(0, 15300000)`（约 4.25 小时），而 Footprint 图初始时 `bounds.size = (0, 0)`，返回 `None`，使用了合理的默认范围。

## 根本原因

### 1. `visible_region` 的计算

```rust
// flowsurface/src/chart.rs:701-711
fn visible_region(&self, size: Size) -> Rectangle {
    let width = size.width / self.scaling;
    let height = size.height / self.scaling;

    Rectangle {
        x: -self.translation.x - width / 2.0,  // 当 translation.x 为正数时，region.x 会是负数
        y: -self.translation.y - height / 2.0,
        width,
        height,
    }
}
```

### 2. `x_to_interval` 对负数的处理

```rust
// flowsurface/src/chart.rs:756-774
fn x_to_interval(&self, x: f32) -> u64 {
    match self.basis {
        Basis::Time(timeframe) => {
            let interval = timeframe.to_milliseconds();

            if x <= 0.0 {
                let diff = (-x / self.cell_width * interval as f32) as u64;
                self.latest_x.saturating_sub(diff)  // 如果 diff 很大，会返回 0
            } else {
                let diff = (x / self.cell_width * interval as f32) as u64;
                self.latest_x.saturating_add(diff)
            }
        }
        ...
    }
}
```

### 3. 问题场景

当 `region.x = -787` 时：
- `x_to_interval(-787)` 进入 `if x <= 0.0` 分支
- `diff = (-(-787) / 4.0 * 1800000)` ≈ `354150000` 毫秒（约 4.1 天）
- 如果 `latest_x` 很小（比如只有 1 根 K 线时），`saturating_sub` 会返回 0
- 所以 `visible_earliest = 0`

### 4. 为什么 Footprint 没问题

- Footprint 初始时 `bounds.size = (0, 0)`
- `visible_region` 返回 `width = 0`
- `visible_timerange` 返回 `None`
- 使用默认范围，这是合理的

### 5. 为什么 Candles 有问题

- Candles 初始时 `bounds.size = (819, 657.5)`（已经有 bounds）
- `x_translation` 的计算基于 `chart.bounds.width`，但初始时可能还没有正确设置
- 导致 `translation.x` 计算错误，`region.x` 为负数且很大
- `x_to_interval(-787)` 返回了 0

## 解决方案

已经在 `missing_data_task` 中添加了验证逻辑：
- 检查 `visible_timerange` 返回的时间范围是否合理
- 如果 `earliest=0` 且 `latest < 1天`，视为无效，使用默认范围

这样可以避免使用错误的时间范围进行数据加载。


