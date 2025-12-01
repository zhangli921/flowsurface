# Footprint 历史数据问题修复

## 问题描述

Footprint 图表无法显示历史数据，只有实时数据。

## 代码分析

### 1. 新架构影响检查

从代码分析来看，**新架构不应该影响历史数据流程**：

```rust
// flowsurface/src/screen/dashboard.rs:972-1010
fn distribute_fetched_data(&mut self, ...) {
    // 新架构：如果 UnifiedDataManager 已启用，先分发数据
    if let Some(data_manager) = &self.unified_data_manager {
        self.distribute_to_unified_manager(&data, &stream_type);
    }
    
    match data {
        FetchedData::Klines { data, req_id } => {
            // 原有的数据插入逻辑仍然执行
            pane_state.insert_hist_klines(req_id, timeframe, ticker_info, &data);
        }
        // ...
    }
}
```

**结论：** 新架构只是将数据缓存到 UnifiedDataManager，原有的数据插入逻辑仍然会执行。

### 2. 历史数据请求流程

历史数据请求在 `missing_data_task` 中触发：

```rust
// flowsurface/src/chart/kline.rs:432-455
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

## 已添加的调试日志

为了诊断问题，我已经添加了调试日志：

1. **在 `missing_data_task` 中**：
   - 记录可见时间范围和 kline 时间范围
   - 记录数据点数量
   - 记录历史数据请求

2. **在 `insert_hist_klines` 中**：
   - 记录插入的 klines 数量
   - 记录插入前后的数据点数量

## 诊断步骤

### 步骤 1: 重新编译并运行

```bash
cd /home/zhangli/Develop/Biance/flow_surface/flowsurface
cargo build --release
FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true ./target/release/flowsurface
```

### 步骤 2: 查看调试日志

```bash
# 实时查看日志
tail -f ~/.local/share/flowsurface/flowsurface-current.log | grep -iE "missing_data_task|insert_hist_klines|requesting"

# 或者查看所有相关日志
grep -iE "missing_data_task|insert_hist_klines|requesting|KlineChart" ~/.local/share/flowsurface/flowsurface-current.log | tail -50
```

### 步骤 3: 检查日志输出

查找以下关键信息：

1. **`missing_data_task` 是否被调用**
   - 应该看到：`KlineChart::missing_data_task: visible=(...), kline=(...), datapoints=...`

2. **历史数据请求是否被发送**
   - 应该看到：`KlineChart::missing_data_task: requesting historical klines, range: ...`

3. **历史数据是否被插入**
   - 应该看到：`KlineChart::insert_hist_klines: req_id=..., klines_count=...`
   - 应该看到：`KlineChart::insert_hist_klines: datapoints before=..., after=...`

## 可能的问题和解决方案

### 问题 1: `missing_data_task` 没有被调用

**症状：** 日志中没有 `missing_data_task` 的输出

**可能原因：**
- `invalidate` 没有被调用
- `visible_timerange` 返回 `None`

**解决方案：**
- 检查 `invalidate` 是否被正确调用
- 检查图表是否已初始化

### 问题 2: 历史数据请求没有被发送

**症状：** 看到 `missing_data_task` 但没有 `requesting historical klines`

**可能原因：**
- `visible_earliest >= kline_earliest`（没有需要的历史数据）
- `request_fetch` 返回 `None`（请求被去重）

**解决方案：**
- 检查时间范围计算是否正确
- 检查 `RequestHandler` 的去重逻辑

### 问题 3: 历史数据请求被发送但没有数据返回

**症状：** 看到 `requesting historical klines` 但没有 `insert_hist_klines`

**可能原因：**
- 网络问题
- 数据下载失败
- 数据为空

**解决方案：**
- 检查网络连接
- 检查数据下载日志
- 检查错误日志

### 问题 4: 历史数据被插入但显示不正确

**症状：** 看到 `insert_hist_klines` 但图表中没有显示

**可能原因：**
- 数据被插入但渲染有问题
- 时间范围不匹配

**解决方案：**
- 检查数据点数量是否增加
- 检查可见时间范围

## 临时解决方案

如果新架构确实影响了历史数据，可以临时禁用新架构：

```bash
# 不设置环境变量，使用原有架构
./target/release/flowsurface
```

如果原有架构正常，说明新架构有问题，需要进一步调查。

## 下一步

1. **重新编译并运行程序**
2. **查看调试日志**，确认问题所在
3. **根据日志输出**，确定具体问题
4. **修复问题**或提供更多信息

## 日志级别设置

如果需要更详细的日志，可以设置环境变量：

```bash
RUST_LOG=debug FLOWSURFACE_ENABLE_UNIFIED_DATA_MANAGER=true ./target/release/flowsurface
```

这将显示所有 DEBUG 级别的日志，包括新添加的调试信息。

