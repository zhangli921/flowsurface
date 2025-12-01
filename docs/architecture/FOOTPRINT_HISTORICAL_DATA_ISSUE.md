# Footprint 历史数据问题诊断

## 问题描述

Footprint 图表无法显示历史数据，只有实时数据。

## 可能的原因

### 1. 新架构影响（需要验证）

新架构在 `distribute_fetched_data` 中会先调用 `distribute_to_unified_manager`，但原有的数据插入逻辑（`insert_hist_klines`）仍然应该执行。

**检查点：**
- `distribute_fetched_data` 中 `insert_hist_klines` 是否被调用
- 新架构是否阻止了数据请求

### 2. 数据请求未触发

`missing_data_task` 负责触发历史数据请求，需要检查：
- `missing_data_task` 是否被正确调用
- `invalidate` 是否被调用
- 数据请求是否被发送

### 3. 数据请求处理问题

即使请求被发送，也可能：
- 请求被去重但未正确处理
- 数据下载失败但未显示错误
- 数据插入失败

## 诊断步骤

### 步骤 1: 检查日志

```bash
# 查看最近的日志
tail -n 100 ~/.local/share/flowsurface/flowsurface-current.log

# 搜索数据请求相关日志
grep -iE "fetch|request|klines|trades" ~/.local/share/flowsurface/flowsurface-current.log | tail -20

# 搜索错误和警告
grep -iE "error|warn|fail" ~/.local/share/flowsurface/flowsurface-current.log | tail -20
```

### 步骤 2: 检查数据请求流程

1. **检查 `missing_data_task` 是否被调用**
   - 在 `invalidate` 方法中，`missing_data_task` 应该被调用
   - 位置：`flowsurface/src/chart/kline.rs:910`

2. **检查数据请求是否被发送**
   - `request_fetch` 应该返回 `Some(Action::RequestFetch(...))`
   - 位置：`flowsurface/src/chart/kline.rs:432-521`

3. **检查数据是否被插入**
   - `insert_hist_klines` 应该被调用
   - 位置：`flowsurface/src/screen/dashboard/pane.rs:360-391`

### 步骤 3: 验证新架构影响

检查新架构是否影响了数据流程：

1. **临时禁用新架构**
   ```bash
   # 不设置环境变量，使用原有架构
   ./target/release/flowsurface
   ```

2. **检查是否仍有问题**
   - 如果原有架构正常，说明新架构有问题
   - 如果原有架构也有问题，说明是其他原因

## 代码检查点

### 1. `distribute_fetched_data` 方法

位置：`flowsurface/src/screen/dashboard.rs:970-1013`

```rust
fn distribute_fetched_data(&mut self, ...) {
    // 新架构：如果 UnifiedDataManager 已启用，先分发数据
    if let Some(data_manager) = &self.unified_data_manager {
        self.distribute_to_unified_manager(&data, &stream_type);
    }
    
    match data {
        FetchedData::Klines { data, req_id } => {
            // 原有的数据插入逻辑应该仍然执行
            pane_state.insert_hist_klines(req_id, timeframe, ticker_info, &data);
        }
        // ...
    }
}
```

**检查：** 新架构不应该阻止原有的数据插入逻辑。

### 2. `missing_data_task` 方法

位置：`flowsurface/src/chart/kline.rs:432-521`

```rust
fn missing_data_task(&mut self) -> Option<Action> {
    // priority 1, basic kline data fetch
    if visible_earliest < kline_earliest {
        let range = FetchRange::Kline(earliest, kline_earliest);
        if let Some(action) = request_fetch(&mut self.request_handler, range) {
            return Some(action);
        }
    }
    // ...
}
```

**检查：** 应该正确检测到需要历史数据并发送请求。

### 3. `insert_hist_klines` 方法

位置：`flowsurface/src/chart/kline.rs:741-773`

```rust
pub fn insert_hist_klines(&mut self, req_id: uuid::Uuid, klines_raw: &[Kline]) {
    match self.data_source {
        PlotData::TimeBased(ref mut timeseries) => {
            timeseries.insert_klines(klines_raw);
            // ...
        }
        // ...
    }
}
```

**检查：** 数据应该被正确插入到 `timeseries`。

## 可能的修复方案

### 方案 1: 如果新架构影响了数据流程

如果新架构阻止了数据请求或插入，需要：
1. 确保新架构不影响原有的数据请求流程
2. 确保 `distribute_fetched_data` 中的原有逻辑仍然执行
3. 检查 UnifiedDataManager 是否错误地阻止了数据请求

### 方案 2: 如果数据请求未触发

如果 `missing_data_task` 没有被调用或没有发送请求：
1. 检查 `invalidate` 是否被正确调用
2. 检查 `visible_timerange` 是否正确计算
3. 添加调试日志来追踪数据请求流程

### 方案 3: 如果数据请求被发送但未处理

如果请求被发送但没有数据返回：
1. 检查网络连接
2. 检查数据下载是否成功
3. 检查数据插入是否成功
4. 检查错误处理逻辑

## 调试建议

1. **添加调试日志**
   - 在 `missing_data_task` 中添加日志
   - 在 `request_fetch` 中添加日志
   - 在 `insert_hist_klines` 中添加日志

2. **检查数据状态**
   - 检查 `timeseries.datapoints` 是否为空
   - 检查 `visible_timerange` 是否正确
   - 检查 `kline_earliest` 和 `kline_latest` 的值

3. **验证数据流程**
   - 确认数据请求被发送
   - 确认数据被下载
   - 确认数据被插入

## 下一步

1. 检查日志文件，查找相关错误或警告
2. 验证新架构是否影响了数据流程
3. 如果新架构有问题，修复或临时禁用
4. 如果问题仍然存在，添加调试日志进行深入诊断

