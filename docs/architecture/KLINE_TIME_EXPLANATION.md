# Kline 时间字段说明

## Kline 结构

```rust
pub struct Kline {
    pub time: u64,        // 开始时间（open_time）
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub close: Price,
    pub volume: (f32, f32),
}
```

## 关键点

### 1. `kline.time` 表示开始时间（Open Time）

`Kline` 结构只有一个 `time` 字段，它表示 **Kline 的开始时间（open_time）**，而不是结束时间。

### 2. 结束时间（Close Time）需要计算

结束时间（close_time）需要通过以下公式计算：

```rust
close_time = kline.time + interval_ms
```

其中 `interval_ms` 是时间间隔（timeframe）对应的毫秒数。

### 3. 时间对齐规则

在 `TimeSeries::insert_trades_or_create_bucket()` 中可以看到，trade 的时间会被对齐到 interval：

```rust
let aggr_time = self.interval.to_milliseconds();
let rounded_time = (trade.time / aggr_time) * aggr_time;
```

这意味着：
- 所有属于同一个 interval 的 trades 会被归到同一个 Kline
- Kline 的 `time` 字段就是这个对齐后的时间戳

### 4. 在代码中的使用

#### 在 `TimeSeries` 中

```rust
// datapoints 使用 kline.time 作为 key
pub datapoints: BTreeMap<u64, D>,

// 插入 kline 时
self.datapoints.entry(kline.time).or_insert_with(|| ...);
```

#### 在完整性检查中

```rust
pub fn check_kline_integrity(&self, earliest: u64, latest: u64, interval: u64) {
    let mut time = earliest;
    while time < latest {
        if !self.datapoints.contains_key(&time) {
            // 缺失的 kline
        }
        time += interval;  // 按 interval 递增
    }
}
```

#### 在 UnifiedDataManager 中

```rust
(FetchedData::Klines { data: klines, .. }, StreamKind::Kline { timeframe, .. }) => {
    let from = klines.first().map(|k| k.time).unwrap_or(0);
    // Kline 的 time 是开始时间，结束时间需要根据 timeframe 计算
    let interval_ms = timeframe.to_milliseconds();
    let to = klines.last().map(|k| k.time + interval_ms).unwrap_or(0);
    (
        FetchRange::Kline(from, to),
        data::chart::Basis::Time(*timeframe),
    )
}
```

## 示例

假设有一个 5 分钟（M5）的 Kline：

```rust
let kline = Kline {
    time: 1609459200000,  // 2021-01-01 00:00:00 UTC (开始时间)
    open: Price::from_f32(30000.0),
    high: Price::from_f32(30100.0),
    low: Price::from_f32(29900.0),
    close: Price::from_f32(30050.0),
    volume: (100.0, 50.0),
};

let interval_ms = Timeframe::M5.to_milliseconds();  // 300000 ms (5分钟)
let open_time = kline.time;                        // 1609459200000
let close_time = kline.time + interval_ms;          // 1609459500000 (2021-01-01 00:05:00 UTC)
```

## 时间范围计算

当需要计算一组 Klines 的时间范围时：

```rust
// 开始时间：第一个 Kline 的开始时间
let from = klines.first().map(|k| k.time).unwrap_or(0);

// 结束时间：最后一个 Kline 的结束时间
let interval_ms = timeframe.to_milliseconds();
let to = klines.last().map(|k| k.time + interval_ms).unwrap_or(0);
```

## 注意事项

1. **时间对齐**：所有 Kline 的时间都是对齐到 interval 边界的
2. **时区**：时间戳通常是 UTC 时间（毫秒）
3. **不重叠**：相邻的 Klines 之间时间不重叠，前一个的 close_time 等于后一个的 open_time

## 相关代码位置

- `flowsurface/exchange/src/lib.rs:594` - Kline 结构定义
- `flowsurface/data/src/aggr/time.rs:210` - 时间对齐逻辑
- `flowsurface/src/screen/dashboard.rs:189` - UnifiedDataManager 中的时间范围计算


