# Footprint vs Candles 图表启动差异分析

## 问题描述
- **Footprint 图**：启动后可以加载多个 K 线
- **Candles 图**：启动后只有一个 K 线

## 关键差异

### 1. Stream 配置差异

#### Footprint Chart
```rust
// flowsurface/src/screen/dashboard/pane.rs:212-222
let streams = by_basis_default(
    derived_plan.basis,
    Timeframe::M5,
    |tf| {
        vec![
            depth_stream(&derived_plan),  // 总是包含 DepthAndTrades
            kline_stream(derived_plan.ticker_info, tf),  // 总是包含 Kline
        ]
    },
    || vec![depth_stream(&derived_plan)],
);
```

#### Candles Chart
```rust
// flowsurface/src/screen/dashboard/pane.rs:250-275
let streams = by_basis_default(
    derived_plan.basis,
    Timeframe::M15,
    |tf| {
        if has_hvn {
            // 只有启用 HVN 时才包含 DepthAndTrades
            vec![
                depth_stream(&derived_plan),
                kline_stream(derived_plan.ticker_info, tf),
            ]
        } else {
            vec![kline_stream(derived_plan.ticker_info, tf)]  // 只有 Kline
        }
    },
    || { ... },
);
```

### 2. 初始数据加载触发

两者都通过 `init_pane` 或 `init_focused_pane` 触发初始数据加载：

```rust
// flowsurface/src/screen/dashboard.rs:864-867
for stream in &streams {
    if let StreamKind::Kline { .. } = stream {
        return kline_fetch_task(self.layout_id, pane_id, *stream, None, None);
    }
}
```

**关键点**：Footprint 和 Candles 都有 `StreamKind::Kline`，所以理论上都应该触发初始数据获取。

### 3. 数据加载范围

`kline_fetch_task` 在 `range` 为 `None` 时，会计算默认范围：

```rust
// flowsurface/src/screen/dashboard.rs:1875-1886
let effective_range = if range.is_none() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let interval_ms = timeframe.to_milliseconds();
    // 计算 7 天前的开始时间（对齐到 interval 边界）
    let days_ago = 7;
    let start_time = now.saturating_sub(days_ago * 24 * 60 * 60 * 1000);
    // 对齐到 interval 边界
    let aligned_start = (start_time / interval_ms) * interval_ms;
    Some((aligned_start, now))
} else {
    range
};
```

**关键点**：初始加载时，两者都应该获取 7 天的数据。

### 4. 数据插入后的处理

在 `distribute_fetched_data` 中，当 `req_id.is_none()`（初始加载）时：

```rust
// flowsurface/src/screen/dashboard.rs:1130-1138
pane_state.insert_hist_klines(req_id, timeframe, ticker_info, &data);

// 初始加载后（req_id 为 None），触发 invalidate 以加载更多历史数据
if req_id.is_none() {
    pane_state.invalidate(Instant::now());
}
```

**关键点**：两者都会在初始加载后触发 `invalidate`。

### 5. `insert_hist_klines` 的处理

当 `req_id` 为 `None` 时，两者都会重新创建整个图表：

```rust
// flowsurface/src/screen/dashboard/pane.rs:375-391
if let Some(id) = req_id {
    chart.insert_hist_klines(id, klines);
} else {
    let (raw_trades, tick_size) = (chart.raw_trades(), chart.tick_size());
    let layout = chart.chart_layout();

    *chart = KlineChart::new(
        layout,
        Basis::Time(timeframe),
        tick_size,
        klines,  // 使用传入的 klines
        raw_trades,
        indicators,
        ticker_info,
        chart.kind(),
    );
}
```

**关键点**：两者都使用相同的逻辑重新创建图表。

## 可能的原因

### 假设 1：数据量差异
- Footprint 的初始数据可能更多（因为总是有 DepthAndTrades stream，可能触发了额外的数据加载）
- Candles 的初始数据可能只有一根 K 线（API 返回的数据量少）

### 假设 2：后续数据加载差异
- Footprint 因为总是有 DepthAndTrades stream，可能通过实时数据触发了更多历史数据加载
- Candles 没有 DepthAndTrades stream，所以不会触发额外的数据加载

### 假设 3：`missing_data_task` 触发差异
- Footprint 可能因为某些条件（如 visible_timerange 计算）更容易触发 `missing_data_task`
- Candles 可能因为条件不满足，没有触发后续数据加载

## 需要进一步调查

1. **检查初始数据量**：在 `insert_hist_klines` 中添加日志，查看 Footprint 和 Candles 初始加载时分别获取了多少根 K 线
2. **检查 `missing_data_task` 触发**：在 `missing_data_task` 中添加日志，查看 Footprint 和 Candles 是否都触发了后续数据加载
3. **检查 `visible_timerange` 计算**：查看 Footprint 和 Candles 的 `visible_timerange` 计算结果是否不同

## 建议的修复方案

如果问题确实是 Candles 图初始数据量不足，可以考虑：

1. **确保初始数据量足够**：在 `kline_fetch_task` 中，确保至少获取一定数量的 K 线（如 100 根）
2. **改进 `missing_data_task`**：在数据量不足时，主动触发更多数据加载
3. **统一 Footprint 和 Candles 的初始化逻辑**：确保两者使用相同的数据加载策略

