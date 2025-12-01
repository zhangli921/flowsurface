# Timeframe 处理逻辑分析

## 原有程序的逻辑

### 1. 数据请求流程

在 `flowsurface/src/screen/dashboard.rs` 中：

```rust
// 步骤 1: 创建 FetchRange（只包含时间范围，不包含 timeframe）
let range = FetchRange::Kline(earliest, kline_earliest);

// 步骤 2: 从 StreamKind 中提取 timeframe
fn kline_fetch_task(
    stream: StreamKind,  // 包含 timeframe
    range: Option<(u64, u64)>,
) -> Task<Message> {
    match stream {
        StreamKind::Kline {
            ticker_info,
            timeframe,  // ✅ 从 stream 中提取 timeframe
        } => Task::perform(
            adapter::fetch_klines(ticker_info, timeframe, range)
            //                                    ^^^^^^^^^ 传递 timeframe
        )
    }
}
```

**结论：原有程序通过 StreamKind 传递 timeframe，确保下载正确的 kline 数据**

### 2. StreamKind 的创建

在 `flowsurface/src/screen/dashboard/pane.rs` 中：

```rust
let kline_stream = |ti: TickerInfo, tf: Timeframe| StreamKind::Kline {
    ticker_info: ti,
    timeframe: tf,  // ✅ timeframe 包含在 StreamKind 中
};

// 根据选定的 basis 创建不同的 stream
let streams = by_basis_default(
    derived_plan.basis,
    Timeframe::M15,  // 默认 timeframe
    |tf| {
        vec![
            kline_stream(derived_plan.ticker_info, tf),  // ✅ 传递选定的 timeframe
        ]
    },
    ...
);
```

**结论：原有程序根据用户选定的 basis（timeframe）创建不同的 StreamKind**

## UnifiedDataManager 中的处理

### 1. DataKey 包含 Basis

```rust
pub struct DataKey {
    pub ticker: TickerInfo,
    pub range: FetchRange,  // 时间范围
    pub basis: Basis,       // ✅ 包含 timeframe (Basis::Time(timeframe))
}
```

**结论：DataKey 通过 `basis` 字段区分不同的 timeframe**

### 2. 在 distribute_to_unified_manager 中

```rust
fn distribute_to_unified_manager(&self, data: &FetchedData, stream_type: &StreamKind) {
    match (data, stream_type) {
        (FetchedData::Klines { data: klines, .. }, StreamKind::Kline { timeframe, .. }) => {
            let from = klines.first().map(|k| k.time).unwrap_or(0);
            let interval_ms = timeframe.to_milliseconds();
            let to = klines.last().map(|k| k.time + interval_ms).unwrap_or(0);
            (
                FetchRange::Kline(from, to),
                data::chart::Basis::Time(*timeframe),  // ✅ 从 stream_type 提取 timeframe
            )
        }
    }
    
    let key = unified_data_manager::DataKey::new(ticker_info, range, basis);
    //                                                              ^^^^^ 包含 timeframe
}
```

**结论：UnifiedDataManager 从 stream_type 中提取 timeframe，并包含在 DataKey 中**

### 3. 缓存区分

由于 `DataKey` 包含 `basis`（timeframe），不同的 timeframe 会有不同的 DataKey：

```rust
// M5 的 kline 数据
DataKey {
    ticker: BTCUSDT,
    range: FetchRange::Kline(1000, 2000),
    basis: Basis::Time(Timeframe::M5),  // ✅ M5
}

// M15 的 kline 数据
DataKey {
    ticker: BTCUSDT,
    range: FetchRange::Kline(1000, 2000),
    basis: Basis::Time(Timeframe::M15),  // ✅ M15 - 不同的 key！
}
```

**结论：不同 timeframe 的数据会被分别缓存，不会混淆**

## 对比分析

### 原有程序

1. **请求时**：`FetchRange::Kline(from, to)` + `StreamKind::Kline { timeframe }`
2. **下载时**：`adapter::fetch_klines(ticker_info, timeframe, range)`
3. **区分方式**：通过 StreamKind 中的 timeframe 区分

### UnifiedDataManager

1. **请求时**：`DataKey { ticker, range, basis }`（basis 包含 timeframe）
2. **缓存时**：不同的 timeframe 会有不同的 DataKey，分别缓存
3. **区分方式**：通过 DataKey 中的 basis 区分

## 结论

✅ **UnifiedDataManager 正确处理了 timeframe**

1. **DataKey 包含 basis**：通过 `Basis::Time(timeframe)` 区分不同的 timeframe
2. **从 stream_type 提取**：在 `distribute_to_unified_manager` 中从 `StreamKind::Kline { timeframe, .. }` 提取
3. **分别缓存**：不同 timeframe 的数据会有不同的 DataKey，不会混淆
4. **与原有逻辑一致**：原有程序通过 StreamKind 传递 timeframe，UnifiedDataManager 通过 DataKey 的 basis 字段传递

## 示例

假设用户有两个图表：
- 图表 A：BTCUSDT，M5 timeframe
- 图表 B：BTCUSDT，M15 timeframe

### 原有程序

```rust
// 图表 A 请求
StreamKind::Kline { ticker_info: BTCUSDT, timeframe: M5 }
FetchRange::Kline(1000, 2000)
→ fetch_klines(BTCUSDT, M5, (1000, 2000))

// 图表 B 请求
StreamKind::Kline { ticker_info: BTCUSDT, timeframe: M15 }
FetchRange::Kline(1000, 2000)
→ fetch_klines(BTCUSDT, M15, (1000, 2000))
```

### UnifiedDataManager

```rust
// 图表 A 请求
DataKey {
    ticker: BTCUSDT,
    range: FetchRange::Kline(1000, 2000),
    basis: Basis::Time(M5),  // ✅ 区分 M5
}

// 图表 B 请求
DataKey {
    ticker: BTCUSDT,
    range: FetchRange::Kline(1000, 2000),
    basis: Basis::Time(M15),  // ✅ 区分 M15 - 不同的 key！
}
```

**两个请求会被分别缓存，不会混淆！**

## 总结

✅ UnifiedDataManager **完全支持**根据 timeframe 下载不同的 kline 数据，与原有程序的逻辑完全一致。

