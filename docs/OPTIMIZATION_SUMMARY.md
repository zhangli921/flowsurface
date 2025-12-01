# Candles 图优化总结

## 优化内容

### 1. 提取常量

将魔法数字提取为常量，提高代码可读性和可维护性：

```rust
const ONE_DAY_MS: u64 = 24 * 60 * 60 * 1000;
const DEFAULT_DAYS_AGO: u64 = 7;
const MIN_DATAPOINTS_FOR_PROACTIVE_FETCH: usize = 10;
const MIN_DATAPOINTS_TO_EXTEND: u64 = 100;
```

### 2. 简化日志级别

- **详细计算过程**：从 `debug` 改为 `trace` 级别
- **关键信息**：保留 `info` 和 `warn` 级别
- **减少噪音**：移除不必要的调试日志

**优化前**：
```rust
log::debug!("[{}] visible_timerange: region=({}, {}), bounds.size={:?}, translation.x={}, latest_x={}, cell_width={}, earliest_raw={}, latest_raw={}, result=({}, {})", ...);
```

**优化后**：
```rust
log::trace!("[{}] visible_timerange: region=({}, {}), translation.x={}, latest_x={}, cell_width={}, earliest_raw={}, latest_raw={}, result=({}, {})", ...);
```

### 3. 提取辅助函数

将重复逻辑提取为辅助函数，提高代码复用性：

#### `is_invalid_timerange`
检查时间范围是否无效（`earliest=0` 且 `latest < 1天`，或 `earliest > latest`）

#### `chart_kind_str`
获取图表类型字符串（用于日志）

#### `calculate_default_timerange`
计算默认时间范围（当 `visible_timerange` 无效时使用）

### 4. 简化验证逻辑

**优化前**：
```rust
let (visible_earliest, visible_latest) = if let Some(range) = self.visible_timerange() {
    let one_day_ms = 24 * 60 * 60 * 1000;
    if range.0 == 0 && range.1 < one_day_ms {
        // 使用默认范围
        ...
    } else {
        range
    }
} else {
    // 使用默认范围
    ...
};
```

**优化后**：
```rust
let (visible_earliest, visible_latest) = self.visible_timerange()
    .filter(|(e, l)| !Self::is_invalid_timerange(*e, *l))
    .unwrap_or_else(|| {
        Self::calculate_default_timerange(kline_earliest, kline_latest, timeframe_ms)
    });
```

### 5. 统一默认范围计算

将默认范围计算逻辑统一到 `calculate_default_timerange` 函数中，避免重复代码。

## 优化效果

1. **代码可读性**：常量命名清晰，逻辑更简洁
2. **可维护性**：辅助函数便于复用和测试
3. **日志清晰度**：减少不必要的调试信息，关键信息更突出
4. **性能**：日志级别优化减少不必要的字符串格式化

## 后续建议

1. **进一步优化**：考虑将 `x_to_interval` 对负数的处理改进，或确保初始化时 `translation.x` 正确设置
2. **单元测试**：为辅助函数添加单元测试
3. **文档**：为关键函数添加更详细的文档注释


