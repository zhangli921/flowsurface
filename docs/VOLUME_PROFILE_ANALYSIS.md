# 筹码峰（Volume Profile）功能分析

## 概述

系统中存在**两套**筹码峰实现：

1. **Heatmap 图的 Volume Profile**：用于热力图，显示价格-成交量分布
2. **Kline 图的 HVN (High Volume Node)**：用于 K 线图，检测高成交量节点

## 1. Heatmap 图的 Volume Profile

### 1.1 数据结构

**位置**：`flowsurface/data/src/chart/heatmap.rs`

```rust
pub enum HeatmapStudy {
    VolumeProfile(ProfileKind),
}

pub enum ProfileKind {
    FixedWindow(usize),    // 固定窗口（数据点数量）
    VisibleRange,          // 可见范围
}
```

### 1.2 计算逻辑

**位置**：`flowsurface/src/chart/heatmap.rs::draw_volume_profile()`

**算法流程**：
1. **确定时间范围**：
   - `VisibleRange`：基于可见区域的时间范围
   - `FixedWindow`：基于最新时间向前推 N 个数据点

2. **数据聚合**：
   - 遍历时间范围内的所有 `HeatmapDataPoint`
   - 按价格档位（`tick_size`）分组
   - 累加每个价格档位的买入/卖出成交量

3. **绘制**：
   - 在图表左侧绘制水平条形图
   - 条形长度与成交量成正比
   - 买入用绿色，卖出用红色

### 1.3 特点

- **实时更新**：基于可见范围或固定窗口动态计算
- **价格分桶**：使用 `tick_size` 进行价格分桶
- **双向显示**：区分买入和卖出成交量

## 2. Kline 图的 HVN (High Volume Node)

### 2.1 数据结构

**位置**：`flowsurface/data/src/chart/kline.rs`

```rust
pub struct HVNResult {
    pub peaks: Vec<HighVolumeNode>,              // 检测到的峰值
    pub poc_volume: f32,                         // POC 成交量
    pub volume_profile: BTreeMap<Price, f32>,    // 价格-成交量映射
}

pub struct HighVolumeNode {
    pub price: Price,
    pub volume: f32,
    pub width: usize,        // 峰值宽度（价格档位数）
    pub strength: f32,       // 强度（相对于 POC）
}
```

### 2.2 计算逻辑

**位置**：`flowsurface/data/src/chart/kline/hvn.rs::HVNCalculator`

**算法流程**（三步法）：

#### 第一步：数据分桶（Bucketing）
```rust
fn bucket_trades(&self, trades_list: &[&KlineTrades]) -> BTreeMap<Price, f32>
```
- 遍历所有 `KlineTrades`
- 按价格分桶（使用 `tick_size` 四舍五入）
- 累加每个价格档位的总成交量（`buy_qty + sell_qty`）

#### 第二步：平滑处理（Smoothing）
```rust
fn smooth_profile(&self, profile: &BTreeMap<Price, f32>, window_size: usize) -> BTreeMap<Price, f32>
```
- 使用移动平均（Moving Average）平滑成交量分布
- 窗口大小必须是奇数（自动调整）
- 对每个价格点，计算周围 `window_size` 个点的平均值
- **目的**：去除噪音，突出主要峰值

#### 第三步：峰值识别（Peak Detection）
```rust
fn detect_peaks(&self, smoothed_profile: &BTreeMap<Price, f32>, absolute_threshold: f32, min_peak_width: usize) -> Vec<HighVolumeNode>
```
- **局部极大值检测**：
  - 条件：`V_i > V_{i-1} AND V_i > V_{i+1}`
  - 允许相等（处理平台情况）
- **阈值过滤**：
  - 使用相对阈值（相对于平滑后的最大值）
  - `absolute_threshold = smoothed_max * (relative_threshold / 100.0)`
- **宽度过滤**：
  - 计算峰值宽度（从峰值点向两侧扩展，直到成交量 < 峰值 * 50%）
  - 只保留宽度 >= `min_peak_width` 的峰值

### 2.3 配置参数

**位置**：`flowsurface/src/modal/pane/settings.rs`

```rust
FootprintStudy::HVN {
    lookback: usize,              // 回看 K 线数量（默认值？）
    smoothing_window: usize,      // 平滑窗口大小（默认值？）
    relative_threshold: u32,      // 相对阈值（10-100%，默认值？）
    min_peak_width: usize,        // 最小峰值宽度（1-10，默认值？）
}
```

### 2.4 绘制逻辑

**位置**：`flowsurface/src/chart/kline.rs::draw_all_hvns()`

**绘制特点**：
1. **完整分布图**：即使没有检测到峰值，也绘制完整的成交量分布
2. **峰值高亮**：检测到的峰值使用更明显的颜色
3. **位置**：
   - **Candles 图**：绘制在 K 线右侧
   - **Footprint 图**：绘制在 clusters 右侧
4. **条形图**：
   - 水平条形，从 `start_x` 向右延伸
   - 条形长度与成交量成正比（归一化到最大成交量）
   - 条形高度 = 价格档位高度

### 2.5 缓存机制

**位置**：`flowsurface/src/chart/kline.rs::HVNCache`

- 使用 `data_hash` 和 `config_hash` 判断缓存是否有效
- TTL 机制（默认值？）
- 避免重复计算

## 3. 关键差异对比

| 特性 | Heatmap Volume Profile | Kline HVN |
|------|------------------------|-----------|
| **用途** | 显示价格-成交量分布 | 检测高成交量节点 |
| **数据源** | `HeatmapDataPoint` | `KlineTrades` |
| **计算方式** | 直接累加 | 三步法（分桶→平滑→峰值检测） |
| **显示方式** | 完整分布图 | 完整分布图 + 峰值标记 |
| **配置** | 固定窗口/可见范围 | 回看数量、平滑窗口、阈值、宽度 |
| **位置** | 图表左侧 | K 线/Clusters 右侧 |

## 4. 潜在问题和改进点

### 4.1 性能问题

1. **HVN 计算**：
   - 每次绘制都可能重新计算（虽然有缓存）
   - 平滑处理需要遍历所有价格档位
   - 峰值检测需要多次遍历

2. **价格档位数量**：
   - 如果价格范围很大，价格档位数量可能非常多
   - HVN 计算中有 `max_price_levels = 1000` 的限制，但可能不够

### 4.2 算法问题

1. **平滑窗口**：
   - 窗口大小必须是奇数，但自动调整可能导致意外行为
   - 边界处理可能不够完善

2. **峰值检测**：
   - 局部极大值检测可能遗漏一些峰值（如平台峰值）
   - 宽度计算使用固定的 50% 阈值，可能不够灵活

3. **阈值计算**：
   - 使用平滑后的最大值计算阈值，但相对阈值是基于 POC 的
   - 可能导致阈值计算不一致

### 4.3 可视化问题

1. **可见范围**：
   - HVN 绘制时，`visible_high` 和 `visible_low` 被设置为 `f32::MAX` 和 `0.0`
   - 实际上会渲染所有 HVN，可能影响性能

2. **位置计算**：
   - `start_x` 和 `end_x` 的计算逻辑复杂，可能不够准确
   - Candles 和 Footprint 的位置计算不一致

3. **颜色和样式**：
   - 峰值和非峰值的颜色区分可能不够明显
   - 条形宽度固定，可能不够灵活

### 4.4 配置问题

1. **默认值**：
   - 配置参数的默认值不明确
   - 用户可能不知道如何调整参数

2. **参数范围**：
   - 某些参数的范围可能不够合理
   - 缺少参数说明和推荐值

## 5. 数据流

### 5.1 Heatmap Volume Profile

```
HeatmapDataPoint (grouped_trades)
    ↓
draw_volume_profile()
    ↓
按价格分桶（tick_size）
    ↓
累加成交量（buy/sell）
    ↓
绘制条形图
```

### 5.2 Kline HVN

```
KlineDataPoint (footprint: KlineTrades)
    ↓
收集可见范围内的所有 KlineTrades
    ↓
HVNCalculator::calculate_hvn()
    ├─ bucket_trades()          # 分桶
    ├─ smooth_profile()         # 平滑
    └─ detect_peaks()           # 峰值检测
    ↓
HVNResult (peaks + volume_profile)
    ↓
draw_all_hvns()
    ↓
绘制完整分布图 + 峰值标记
```

## 6. 相关代码位置

### 6.1 数据结构
- `flowsurface/data/src/chart/kline.rs`：`HVNResult`, `HighVolumeNode`, `HVNCalculator`
- `flowsurface/data/src/chart/heatmap.rs`：`HeatmapStudy`, `ProfileKind`

### 6.2 计算逻辑
- `flowsurface/data/src/chart/kline/hvn.rs`：HVN 计算器实现
- `flowsurface/src/chart/heatmap.rs::draw_volume_profile()`：Heatmap 筹码峰绘制

### 6.3 绘制逻辑
- `flowsurface/src/chart/kline.rs::draw_all_hvns()`：HVN 绘制
- `flowsurface/src/chart/heatmap.rs::draw_volume_profile()`：Heatmap 筹码峰绘制

### 6.4 配置 UI
- `flowsurface/src/modal/pane/settings.rs`：HVN 和 Volume Profile 的配置界面

## 7. 总结

筹码峰功能分为两个独立的实现：

1. **Heatmap Volume Profile**：简单直接，用于实时显示价格-成交量分布
2. **Kline HVN**：复杂算法，用于检测和标记高成交量节点

两者都使用价格分桶和成交量累加，但 HVN 增加了平滑和峰值检测步骤，更适合识别关键支撑/阻力位。

**主要改进方向**：
- 性能优化（缓存、采样）
- 算法改进（峰值检测、阈值计算）
- 可视化改进（可见范围裁剪、样式优化）
- 配置改进（默认值、参数说明）


