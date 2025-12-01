# HVN 功能重新设计分析

## 用户需求

用户希望实现一个**屏幕可见范围内的筹码山峰显示**，类似图片中右侧的垂直 Volume Profile：
- 基于**屏幕可见范围**（时间和价格）
- 简单的成交量分布图（山峰形状）
- 垂直显示在图表右侧
- 颜色渐变表示不同价格范围

## 当前实现的问题

### 1. 数据范围问题

**当前实现**：
```rust
// 使用 lookback 参数，基于固定的 K 线数量
let trades_list: Vec<&KlineTrades> = timeseries
    .datapoints
    .iter()
    .rev()
    .take(lookback)  // ❌ 固定数量，不是可见范围
    .map(|(_, dp)| &dp.footprint)
    .collect();
```

**问题**：
- 使用 `lookback` 参数，基于固定的 K 线数量
- 不随屏幕缩放/平移而动态更新
- 可能包含不可见的数据，或遗漏可见的数据

**应该改为**：
```rust
// 基于可见时间范围
let visible_time_range = visible_earliest..=visible_latest;
let trades_list: Vec<&KlineTrades> = timeseries
    .datapoints
    .range(visible_time_range)  // ✅ 基于可见范围
    .map(|(_, dp)| &dp.footprint)
    .collect();
```

### 2. 价格范围问题

**当前实现**：
```rust
// 获取可见价格范围（只渲染可见范围内的 HVN）
let (visible_high, visible_low) = {
    let chart = &frame.size();
    // 这里需要从frame获取可见范围，简化处理：渲染所有HVN
    (Price::from_f32(f32::MAX), Price::from_f32(0.0))  // ❌ 渲染所有价格
};
```

**问题**：
- 没有正确获取可见价格范围
- 渲染所有价格档位，影响性能
- 可能显示屏幕外的数据

**应该改为**：
```rust
// 从 region 获取可见价格范围
let region = chart.visible_region(chart.bounds.size());
let (visible_high, visible_low) = chart.price_range(&region);  // ✅ 基于可见范围
```

### 3. 算法复杂度问题

**当前实现**：
- 三步法：分桶 → 平滑 → 峰值检测
- 使用复杂的峰值检测算法
- 有阈值和宽度过滤

**问题**：
- 对于简单的成交量分布图，算法过于复杂
- 平滑处理可能改变原始分布
- 峰值检测可能遗漏或误判

**应该改为**：
- 简单的分桶累加即可
- 不需要平滑和峰值检测
- 直接显示原始成交量分布

### 4. 绘制位置和方式问题

**当前实现**：
```rust
// 绘制在 K 线/Clusters 右侧
let start_x = rightmost_x + (candle_width / 2.0) + spacing.candle_to_cluster;
// 水平条形，从 start_x 向右延伸
frame.fill_rectangle(
    Point::new(start_x, y_position - bar_height / 2.0),
    Size::new(bar_length, bar_height),  // 水平条形
    bar_color,
);
```

**问题**：
- 绘制位置计算复杂
- 水平条形，不是垂直的山峰形状
- 位置可能不够固定（随 K 线位置变化）

**应该改为**：
- 固定在图表右侧边缘
- 垂直条形，从右边缘向左延伸
- 类似 Heatmap 的 Volume Profile 绘制方式

### 5. 颜色渐变问题

**当前实现**：
```rust
let bar_color = if is_peak {
    palette.primary.strong.color.scale_alpha(0.9)
} else {
    palette.primary.weak.color.scale_alpha(0.6)
};
```

**问题**：
- 只有峰值和非峰值的两种颜色
- 没有价格范围的渐变效果
- 不够直观

**应该改为**：
- 根据价格位置使用颜色渐变
- 低价格用白色/浅色，高价格用黄色/深色
- 类似 Heatmap 的颜色方案

## 改进方案

### 方案 1：简化现有 HVN 实现

**优点**：
- 复用现有代码结构
- 保持配置界面

**缺点**：
- 需要大幅修改现有逻辑
- 可能影响其他功能

### 方案 2：参考 Heatmap Volume Profile 实现

**优点**：
- Heatmap 的实现已经符合需求
- 代码清晰简单
- 可以直接复用

**缺点**：
- 需要适配 K 线图的数据结构
- 需要处理不同的绘制上下文

### 方案 3：创建新的简化实现

**优点**：
- 完全符合需求
- 代码简洁
- 不影响现有功能

**缺点**：
- 需要新增代码
- 可能重复部分逻辑

## 推荐方案

**推荐方案 2 + 方案 3 的混合**：
1. 参考 Heatmap Volume Profile 的简单实现
2. 创建新的简化函数，专门用于可见范围的筹码峰
3. 保留现有 HVN 作为可选的高级功能

### 实现要点

1. **数据收集**：
   ```rust
   // 基于可见时间范围
   let visible_time_range = visible_earliest..=visible_latest;
   let trades_list = timeseries
       .datapoints
       .range(visible_time_range)
       .map(|(_, dp)| &dp.footprint)
       .collect();
   ```

2. **价格范围**：
   ```rust
   // 基于可见价格范围
   let region = chart.visible_region(chart.bounds.size());
   let (visible_high, visible_low) = chart.price_range(&region);
   ```

3. **简单分桶**：
   ```rust
   // 直接累加，不需要平滑
   let mut volume_profile = BTreeMap::new();
   for kline_trades in trades_list {
       for (price, group) in &kline_trades.trades {
           if price >= visible_low && price <= visible_high {
               let rounded_price = price.round_to_step(tick_size);
               *volume_profile.entry(rounded_price).or_insert(0.0) += 
                   group.buy_qty + group.sell_qty;
           }
       }
   }
   ```

4. **绘制位置**：
   ```rust
   // 固定在图表右侧
   let profile_x = region.x + region.width;  // 右边缘
   let profile_width = area_width;  // 固定宽度
   ```

5. **垂直条形**：
   ```rust
   // 从右边缘向左延伸
   frame.fill_rectangle(
       Point::new(profile_x - bar_length, y_position - bar_height / 2.0),
       Size::new(bar_length, bar_height),
       bar_color,
   );
   ```

6. **颜色渐变**：
   ```rust
   // 根据价格位置计算颜色
   let price_ratio = (price.to_f32() - visible_low.to_f32()) / 
                     (visible_high.to_f32() - visible_low.to_f32());
   let color = interpolate_color(price_ratio);  // 从白色到黄色渐变
   ```

## 配置调整

### 当前配置
```rust
FootprintStudy::HVN {
    lookback: usize,              // ❌ 不需要
    smoothing_window: usize,      // ❌ 不需要
    relative_threshold: u32,      // ❌ 不需要
    min_peak_width: usize,        // ❌ 不需要
}
```

### 新配置（可选）
```rust
FootprintStudy::VolumeProfile {
    area_width: f32,              // 筹码峰宽度（像素）
    color_scheme: ColorScheme,    // 颜色方案
}
```

或者完全不需要配置，使用默认值。

## 代码位置

### 需要修改的文件
1. `flowsurface/src/chart/kline.rs::draw_all_hvns()` - 重写绘制逻辑
2. `flowsurface/data/src/chart/kline/hvn.rs` - 简化计算逻辑，或创建新函数
3. `flowsurface/src/modal/pane/settings.rs` - 调整配置界面（如果需要）

### 参考实现
1. `flowsurface/src/chart/heatmap.rs::draw_volume_profile()` - 参考绘制方式
2. `flowsurface/src/chart/heatmap.rs::draw_volume_bar()` - 参考条形绘制

## 总结

当前 HVN 实现的主要偏差：
1. ❌ 使用固定 `lookback`，不是可见范围
2. ❌ 没有正确使用可见价格范围
3. ❌ 算法过于复杂（平滑+峰值检测）
4. ❌ 绘制方式不够直观（水平条形，位置不固定）
5. ❌ 颜色方案简单（只有两种颜色）

改进方向：
1. ✅ 基于屏幕可见范围（时间和价格）
2. ✅ 简单的分桶累加
3. ✅ 垂直条形，固定在右侧
4. ✅ 颜色渐变表示价格范围
5. ✅ 类似 Heatmap Volume Profile 的实现


