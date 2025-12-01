# 筹码峰（Volume Profile）架构设计方案

## 需求分析

### 功能需求
1. **可见范围筹码峰**：基于屏幕可见的时间范围和价格范围
2. **固定时间区间筹码峰**：基于固定的时间窗口（未来扩展）
3. **高性能**：数据量大，需要优化计算性能

### 性能要求
- 数据量大（可能数千个 K 线，数万个交易）
- 实时更新（屏幕缩放/平移时需要重新计算）
- 低延迟（不影响 UI 响应）

## 架构设计

### 1. 核心抽象

#### 1.1 数据范围抽象（Range Strategy）

```rust
/// 数据范围策略（Strategy Pattern）
pub enum VolumeProfileRange {
    /// 可见范围：基于屏幕可见的时间和价格范围
    Visible {
        time_range: RangeInclusive<u64>,
        price_range: (Price, Price),
    },
    /// 固定时间区间：基于固定的时间窗口（未来扩展）
    FixedWindow {
        start_time: u64,
        end_time: u64,
        // 可选：价格范围限制
        price_range: Option<(Price, Price)>,
    },
}

impl VolumeProfileRange {
    /// 检查范围是否有效
    pub fn is_valid(&self) -> bool {
        match self {
            VolumeProfileRange::Visible { time_range, .. } => {
                time_range.start() <= time_range.end()
            }
            VolumeProfileRange::FixedWindow { start_time, end_time, .. } => {
                start_time <= end_time
            }
        }
    }
    
    /// 获取时间范围
    pub fn time_range(&self) -> RangeInclusive<u64> {
        match self {
            VolumeProfileRange::Visible { time_range, .. } => time_range.clone(),
            VolumeProfileRange::FixedWindow { start_time, end_time, .. } => {
                *start_time..=*end_time
            }
        }
    }
    
    /// 获取价格范围（如果有限制）
    pub fn price_range(&self) -> Option<(Price, Price)> {
        match self {
            VolumeProfileRange::Visible { price_range, .. } => Some(*price_range),
            VolumeProfileRange::FixedWindow { price_range, .. } => *price_range,
        }
    }
    
    /// 计算范围哈希（用于缓存键）
    pub fn hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        match self {
            VolumeProfileRange::Visible { time_range, price_range } => {
                time_range.start().hash(&mut hasher);
                time_range.end().hash(&mut hasher);
                price_range.0.hash(&mut hasher);
                price_range.1.hash(&mut hasher);
            }
            VolumeProfileRange::FixedWindow { start_time, end_time, price_range } => {
                start_time.hash(&mut hasher);
                end_time.hash(&mut hasher);
                if let Some((low, high)) = price_range {
                    low.hash(&mut hasher);
                    high.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }
}
```

#### 1.2 计算结果抽象

```rust
/// 筹码峰计算结果
#[derive(Debug, Clone)]
pub struct VolumeProfileResult {
    /// 价格档位到成交量的映射
    pub profile: BTreeMap<Price, f32>,
    /// 最大成交量（用于归一化）
    pub max_volume: f32,
    /// 总成交量
    pub total_volume: f32,
    /// 价格范围
    pub price_range: (Price, Price),
    /// 时间范围
    pub time_range: (u64, u64),
    /// 数据点数量
    pub datapoint_count: usize,
}

impl VolumeProfileResult {
    pub fn new() -> Self {
        Self {
            profile: BTreeMap::new(),
            max_volume: 0.0,
            total_volume: 0.0,
            price_range: (Price::from_f32(0.0), Price::from_f32(0.0)),
            time_range: (0, 0),
            datapoint_count: 0,
        }
    }
    
    /// 检查结果是否为空
    pub fn is_empty(&self) -> bool {
        self.profile.is_empty() || self.max_volume == 0.0
    }
}
```

### 2. 计算引擎

#### 2.1 计算器接口

```rust
/// 筹码峰计算器（Trait）
pub trait VolumeProfileCalculator {
    /// 计算筹码峰
    fn calculate(
        &self,
        data_source: &PlotData<KlineDataPoint>,
        range: &VolumeProfileRange,
        tick_size: PriceStep,
    ) -> VolumeProfileResult;
    
    /// 增量更新（如果数据源支持）
    fn update_incremental(
        &self,
        result: &mut VolumeProfileResult,
        new_data: &[KlineDataPoint],
        range: &VolumeProfileRange,
        tick_size: PriceStep,
    ) -> bool;
}
```

#### 2.2 简单计算器实现

```rust
/// 简单计算器：直接累加，适合可见范围
pub struct SimpleVolumeProfileCalculator;

impl VolumeProfileCalculator for SimpleVolumeProfileCalculator {
    fn calculate(
        &self,
        data_source: &PlotData<KlineDataPoint>,
        range: &VolumeProfileRange,
        tick_size: PriceStep,
    ) -> VolumeProfileResult {
        let time_range = range.time_range();
        let price_range_opt = range.price_range();
        
        let mut profile = BTreeMap::new();
        let mut total_volume = 0.0;
        let mut datapoint_count = 0;
        let mut min_price = Price::from_f32(f32::MAX);
        let mut max_price = Price::from_f32(0.0);
        
        // 收集数据
        let datapoints: Vec<&KlineDataPoint> = match data_source {
            PlotData::TimeBased(timeseries) => {
                timeseries
                    .datapoints
                    .range(time_range.clone())
                    .map(|(_, dp)| dp)
                    .collect()
            }
            PlotData::TickBased(tick_aggr) => {
                tick_aggr
                    .datapoints
                    .iter()
                    .filter(|dp| {
                        let t = dp.kline.time;
                        *time_range.start() <= t && t <= *time_range.end()
                    })
                    .collect()
            }
        };
        
        datapoint_count = datapoints.len();
        
        // 累加成交量
        for dp in datapoints {
            for (price, group) in &dp.footprint.trades {
                // 价格范围过滤
                if let Some((low, high)) = price_range_opt {
                    if *price < low || *price > high {
                        continue;
                    }
                }
                
                let rounded_price = price.round_to_step(tick_size);
                let volume = group.buy_qty + group.sell_qty;
                
                if volume > 0.0 {
                    *profile.entry(rounded_price).or_insert(0.0) += volume;
                    total_volume += volume;
                    
                    if rounded_price < min_price {
                        min_price = rounded_price;
                    }
                    if rounded_price > max_price {
                        max_price = rounded_price;
                    }
                }
            }
        }
        
        let max_volume = profile.values().copied().fold(0.0, f32::max);
        
        VolumeProfileResult {
            profile,
            max_volume,
            total_volume,
            price_range: if min_price <= max_price {
                (min_price, max_price)
            } else {
                (Price::from_f32(0.0), Price::from_f32(0.0))
            },
            time_range: (*time_range.start(), *time_range.end()),
            datapoint_count,
        }
    }
    
    fn update_incremental(
        &self,
        result: &mut VolumeProfileResult,
        new_data: &[KlineDataPoint],
        range: &VolumeProfileRange,
        tick_size: PriceStep,
    ) -> bool {
        // 简单实现：不支持增量更新，返回 false
        false
    }
}
```

### 3. 缓存策略

#### 3.1 缓存键设计

```rust
/// 缓存键
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VolumeProfileCacheKey {
    /// 数据范围哈希
    range_hash: u64,
    /// 数据哈希（基于实际数据内容）
    data_hash: u64,
    /// tick_size 哈希
    tick_size_hash: u64,
}

impl VolumeProfileCacheKey {
    pub fn new(
        range: &VolumeProfileRange,
        data_source: &PlotData<KlineDataPoint>,
        tick_size: PriceStep,
    ) -> Self {
        let range_hash = range.hash();
        let data_hash = Self::hash_data_source(data_source, range);
        let tick_size_hash = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            tick_size.units.hash(&mut hasher);
            hasher.finish()
        };
        
        Self {
            range_hash,
            data_hash,
            tick_size_hash,
        }
    }
    
    /// 计算数据源哈希（只计算相关范围的数据）
    fn hash_data_source(
        data_source: &PlotData<KlineDataPoint>,
        range: &VolumeProfileRange,
    ) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        let time_range = range.time_range();
        
        match data_source {
            PlotData::TimeBased(timeseries) => {
                // 只哈希时间范围内的数据点数量和时间戳
                let count = timeseries
                    .datapoints
                    .range(time_range.clone())
                    .count();
                count.hash(&mut hasher);
                
                // 哈希第一个和最后一个时间戳
                if let Some((first_time, _)) = timeseries
                    .datapoints
                    .range(time_range.clone())
                    .next()
                {
                    first_time.hash(&mut hasher);
                }
                if let Some((last_time, _)) = timeseries
                    .datapoints
                    .range(time_range.clone())
                    .next_back()
                {
                    last_time.hash(&mut hasher);
                }
            }
            PlotData::TickBased(tick_aggr) => {
                let count = tick_aggr
                    .datapoints
                    .iter()
                    .filter(|dp| {
                        let t = dp.kline.time;
                        *time_range.start() <= t && t <= *time_range.end()
                    })
                    .count();
                count.hash(&mut hasher);
            }
        }
        
        hasher.finish()
    }
}
```

#### 3.2 缓存管理器

```rust
use std::sync::{Arc, RwLock};
use std::collections::HashMap;
use std::time::Instant;

/// 筹码峰缓存管理器
pub struct VolumeProfileCache {
    /// 缓存存储
    cache: Arc<RwLock<HashMap<VolumeProfileCacheKey, CachedResult>>>,
    /// 最大缓存条目数
    max_entries: usize,
    /// TTL（毫秒）
    ttl_ms: u64,
}

struct CachedResult {
    result: VolumeProfileResult,
    created_at: Instant,
}

impl VolumeProfileCache {
    pub fn new(max_entries: usize, ttl_ms: u64) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            max_entries,
            ttl_ms,
        }
    }
    
    /// 获取缓存结果
    pub fn get(&self, key: &VolumeProfileCacheKey) -> Option<VolumeProfileResult> {
        let cache = self.cache.read().unwrap();
        if let Some(cached) = cache.get(key) {
            let elapsed = cached.created_at.elapsed().as_millis() as u64;
            if elapsed < self.ttl_ms {
                return Some(cached.result.clone());
            }
        }
        None
    }
    
    /// 设置缓存结果
    pub fn set(&self, key: VolumeProfileCacheKey, result: VolumeProfileResult) {
        let mut cache = self.cache.write().unwrap();
        
        // 如果缓存已满，清理过期条目或最旧的条目
        if cache.len() >= self.max_entries {
            self.cleanup(&mut cache);
        }
        
        cache.insert(key, CachedResult {
            result,
            created_at: Instant::now(),
        });
    }
    
    /// 清理过期或最旧的条目
    fn cleanup(&self, cache: &mut HashMap<VolumeProfileCacheKey, CachedResult>) {
        let now = Instant::now();
        let mut to_remove = Vec::new();
        
        // 收集过期条目
        for (key, cached) in cache.iter() {
            let elapsed = cached.created_at.elapsed().as_millis() as u64;
            if elapsed >= self.ttl_ms {
                to_remove.push(key.clone());
            }
        }
        
        // 移除过期条目
        for key in to_remove {
            cache.remove(&key);
        }
        
        // 如果还是满的，移除最旧的条目
        if cache.len() >= self.max_entries {
            let mut entries: Vec<_> = cache.iter().collect();
            entries.sort_by_key(|(_, cached)| cached.created_at);
            
            // 移除最旧的 10%
            let remove_count = (self.max_entries / 10).max(1);
            for (key, _) in entries.iter().take(remove_count) {
                cache.remove(key);
            }
        }
    }
    
    /// 清除所有缓存
    pub fn clear(&self) {
        let mut cache = self.cache.write().unwrap();
        cache.clear();
    }
}
```

### 4. 主管理器

```rust
/// 筹码峰管理器（统一入口）
pub struct VolumeProfileManager {
    /// 计算器
    calculator: Box<dyn VolumeProfileCalculator>,
    /// 缓存
    cache: VolumeProfileCache,
}

impl VolumeProfileManager {
    pub fn new() -> Self {
        Self {
            calculator: Box::new(SimpleVolumeProfileCalculator),
            cache: VolumeProfileCache::new(100, 5000), // 100 条目，5 秒 TTL
        }
    }
    
    /// 计算筹码峰（带缓存）
    pub fn calculate(
        &self,
        data_source: &PlotData<KlineDataPoint>,
        range: VolumeProfileRange,
        tick_size: PriceStep,
    ) -> VolumeProfileResult {
        // 检查缓存
        let cache_key = VolumeProfileCacheKey::new(&range, data_source, tick_size);
        if let Some(cached_result) = self.cache.get(&cache_key) {
            return cached_result;
        }
        
        // 计算
        let result = self.calculator.calculate(data_source, &range, tick_size);
        
        // 缓存结果
        self.cache.set(cache_key, result.clone());
        
        result
    }
    
    /// 清除缓存
    pub fn clear_cache(&self) {
        self.cache.clear();
    }
}
```

### 5. 性能优化策略

#### 5.1 数据采样（当数据量过大时）

```rust
impl SimpleVolumeProfileCalculator {
    /// 采样数据点（如果数据量过大）
    fn sample_datapoints(
        datapoints: &[&KlineDataPoint],
        max_samples: usize,
    ) -> Vec<&KlineDataPoint> {
        if datapoints.len() <= max_samples {
            return datapoints.to_vec();
        }
        
        // 均匀采样
        let step = datapoints.len() / max_samples;
        datapoints
            .iter()
            .step_by(step.max(1))
            .take(max_samples)
            .copied()
            .collect()
    }
}
```

#### 5.2 价格档位限制

```rust
impl SimpleVolumeProfileCalculator {
    /// 限制价格档位数量（如果价格范围过大）
    fn limit_price_levels(
        profile: &mut BTreeMap<Price, f32>,
        max_levels: usize,
    ) {
        if profile.len() <= max_levels {
            return;
        }
        
        // 保留成交量最大的档位
        let mut sorted: Vec<_> = profile.iter().collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());
        sorted.truncate(max_levels);
        
        let keep_prices: std::collections::HashSet<_> = 
            sorted.iter().map(|(p, _)| **p).collect();
        
        profile.retain(|p, _| keep_prices.contains(p));
    }
}
```

#### 5.3 并行计算（可选）

```rust
use rayon::prelude::*;

impl SimpleVolumeProfileCalculator {
    /// 并行累加（使用 rayon）
    fn calculate_parallel(
        &self,
        datapoints: &[&KlineDataPoint],
        price_range_opt: Option<(Price, Price)>,
        tick_size: PriceStep,
    ) -> BTreeMap<Price, f32> {
        // 并行处理每个数据点
        let profiles: Vec<BTreeMap<Price, f32>> = datapoints
            .par_iter()
            .map(|dp| {
                let mut profile = BTreeMap::new();
                for (price, group) in &dp.footprint.trades {
                    if let Some((low, high)) = price_range_opt {
                        if *price < low || *price > high {
                            continue;
                        }
                    }
                    
                    let rounded_price = price.round_to_step(tick_size);
                    let volume = group.buy_qty + group.sell_qty;
                    if volume > 0.0 {
                        *profile.entry(rounded_price).or_insert(0.0) += volume;
                    }
                }
                profile
            })
            .collect();
        
        // 合并结果
        let mut result = BTreeMap::new();
        for profile in profiles {
            for (price, volume) in profile {
                *result.entry(price).or_insert(0.0) += volume;
            }
        }
        
        result
    }
}
```

## 使用示例

```rust
// 在 KlineChart 中使用
impl KlineChart {
    fn draw_volume_profile(
        &self,
        frame: &mut canvas::Frame,
        chart: &ViewState,
        region: &Rectangle,
    ) {
        // 1. 确定数据范围
        let visible_time_range = {
            let earliest = chart.x_to_interval(region.x);
            let latest = chart.x_to_interval(region.x + region.width);
            earliest..=latest
        };
        
        let (visible_high, visible_low) = chart.price_range(region);
        
        let range = VolumeProfileRange::Visible {
            time_range: visible_time_range,
            price_range: (visible_high, visible_low),
        };
        
        // 2. 计算筹码峰
        let manager = VolumeProfileManager::new();
        let result = manager.calculate(
            &self.data_source,
            range,
            self.chart.tick_size,
        );
        
        // 3. 绘制
        if !result.is_empty() {
            self.draw_profile_bars(frame, &result, region);
        }
    }
}
```

## 架构优势

1. **可扩展性**：
   - Strategy Pattern：易于添加新的范围类型
   - Trait 抽象：易于替换计算器实现

2. **高性能**：
   - 智能缓存：基于数据范围和内容哈希
   - 数据采样：处理大数据量
   - 并行计算：可选的多线程加速

3. **可维护性**：
   - 清晰的职责分离
   - 易于测试和调试
   - 代码复用

4. **未来扩展**：
   - 固定时间区间：只需添加新的 `VolumeProfileRange` 变体
   - 增量更新：在 `VolumeProfileCalculator` trait 中实现
   - 其他优化：在计算器中添加

## 实现步骤

1. **Phase 1**：实现核心抽象和简单计算器
2. **Phase 2**：实现缓存管理器
3. **Phase 3**：集成到 KlineChart
4. **Phase 4**：性能优化（采样、并行）
5. **Phase 5**：实现固定时间区间（未来）


