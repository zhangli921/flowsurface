# 时间戳对齐问题分析

## 问题描述
筹码峰、K线、纵坐标、横坐标没有对齐，怀疑是数据层、VP计算层、渲染层的时间戳没有对齐。

## 时间戳单位分析

### 1. 数据层
- **`data::KLine.open_time_us`**: 微秒 (u64)
- **`exchange::Kline.time`**: 毫秒 (u64) - 从交易所API返回
- **`TimeSeries` 的 key**: 毫秒 (u64) - 使用 `kline.time`

### 2. VP计算层
- **`TimeRange.start_us`**: 微秒 (u64)
- **`TimeRange.end_us`**: 微秒 (u64)
- **`visible_time_range_us()`**: 
  - 调用 `interval_range()` 返回毫秒
  - 然后乘以 1000 转换为微秒

### 3. 渲染层
- **`ChartState.latest_x`**: 毫秒 (u64) - 从 `timeseries.latest_timestamp()` 获取
- **`KlineRenderer.base_time_ms`**: 毫秒 (f64) - 从 `k.open_time_us / 1_000` 计算
- **K线时间戳转换**: `time_ms = (k.open_time_us / 1_000) as f64`

## 潜在问题

### 问题1: `latest_x` 的单位不一致
- `latest_x` 来自 `timeseries.latest_timestamp()`，返回的是毫秒
- 但 `timeseries` 的 key 是 `kline.time`（毫秒）
- 当插入新的 KLine 时，`kline.time` 可能和 `open_time_us / 1000` 不一致

### 问题2: `visible_time_range_us()` 的计算可能不准确
- `interval_range()` 基于 `x_to_interval()`，它使用 `latest_x`（毫秒）
- 但 `x_to_interval()` 的计算可能和渲染时的坐标转换不一致

### 问题3: VP计算时的时间范围可能和渲染时不一致
- VP计算使用 `visible_time_range_us()` 返回的微秒范围
- 但渲染时使用 `base_time_ms` 和 `latest_x`（毫秒）
- 可能存在精度损失或舍入误差

## 需要检查的关键点

1. **`latest_x` 的更新逻辑**：
   - 检查 `timeseries.latest_timestamp()` 返回的值是否和实际K线数据一致
   - 检查 `kline.time` 是否等于 `open_time_us / 1000`

2. **`visible_time_range_us()` 的计算**：
   - 检查 `interval_range()` 的计算是否准确
   - 检查 `x_to_interval()` 和渲染时的坐标转换是否一致

3. **VP计算时的时间范围**：
   - 检查 `fetch_ticks_blocking()` 是否正确使用微秒范围
   - 检查 tick 数据的时间戳单位

4. **渲染时的坐标转换**：
   - 检查 `base_time_ms` 的计算是否准确
   - 检查时间偏移的计算是否和 `x_to_interval()` 一致

## 建议的修复方案

1. **统一时间戳单位**：
   - 在数据层统一使用微秒
   - 在渲染层统一使用毫秒
   - 确保转换时使用正确的乘除因子

2. **添加调试日志**：
   - 记录 `visible_time_range_us()` 的计算过程
   - 记录 VP计算时使用的时间范围
   - 记录渲染时使用的时间范围

3. **验证对齐**：
   - 在 VP计算和渲染时使用相同的时间范围计算逻辑
   - 确保坐标转换的一致性

