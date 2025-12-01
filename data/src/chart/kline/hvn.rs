use exchange::util::{Price, PriceStep};
use std::collections::BTreeMap;

use super::{HighVolumeNode, HVNResult, KlineTrades};

/// HVN 计算器
pub struct HVNCalculator {
    /// 价格步长（用于分桶）
    tick_size: PriceStep,
}

impl HVNCalculator {
    pub fn new(tick_size: PriceStep) -> Self {
        Self { tick_size }
    }

    /// 从多个 KlineTrades 计算 HVN
    ///
    /// # 参数
    /// - `trades_list`: 一段时间内的所有 KlineTrades
    /// - `smoothing_window`: 平滑窗口大小
    /// - `relative_threshold`: 相对阈值（相对于POC，0-100，表示百分比）
    /// - `min_peak_width`: 最小峰值宽度（价格档位数）
    pub fn calculate_hvn(
        &self,
        trades_list: &[&KlineTrades],
        smoothing_window: usize,
        relative_threshold: u32,
        min_peak_width: usize,
    ) -> HVNResult {
        // 限制 lookback 最大值，防止性能问题
        let max_price_levels = 1000;
        
        // 第一步：数据分桶（Bucketing）
        let mut volume_profile = self.bucket_trades(trades_list);

        if volume_profile.is_empty() {
            return HVNResult::default();
        }

        // 如果价格档位太多，进行采样（性能优化）
        if volume_profile.len() > max_price_levels {
            let mut sorted: Vec<_> = volume_profile.iter().collect();
            sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());
            sorted.truncate(max_price_levels);

            volume_profile = sorted
                .into_iter()
                .map(|(&p, &v)| (p, v))
                .collect();
        }

        // 计算 POC（用于相对阈值）
        let poc_volume = volume_profile
            .values()
            .copied()
            .fold(0.0, f32::max);

        if poc_volume == 0.0 {
            return HVNResult::default();
        }

        // 第二步：平滑处理（Smoothing）
        let smoothed_profile = self.smooth_profile(&volume_profile, smoothing_window);
        
        // 计算平滑后的最大值（用于阈值计算）
        let smoothed_max = smoothed_profile
            .values()
            .copied()
            .fold(0.0, f32::max);

        // 第三步：识别峰值（Peak Detection）
        // 阈值应该基于平滑后的最大值，因为我们要检测的是平滑后的峰值
        // 这样可以确保即使平滑窗口较大，也能正确检测到峰值
        let threshold_ratio = relative_threshold as f32 / 100.0;
        let absolute_threshold = smoothed_max * threshold_ratio;
        
        let peaks = self.detect_peaks(
            &smoothed_profile,
            absolute_threshold,
            min_peak_width,
        );
        
        let result = HVNResult {
            peaks: peaks.clone(), // 克隆以确保返回正确的值
            poc_volume,
            volume_profile,
        };
        
        result
    }

    /// 第一步：数据分桶
    /// 将所有交易数据按价格分桶，累加每个桶的成交量
    fn bucket_trades(&self, trades_list: &[&KlineTrades]) -> BTreeMap<Price, f32> {
        let mut volume_profile = BTreeMap::new();

        for kline_trades in trades_list {
            for (price, group) in &kline_trades.trades {
                // 将价格四舍五入到最近的 tick_size
                let rounded_price = price.round_to_step(self.tick_size);

                // 累加该价格档位的成交量
                *volume_profile.entry(rounded_price).or_insert(0.0) += group.total_qty();
            }
        }

        volume_profile
    }

    /// 第二步：平滑处理
    /// 使用移动平均（Moving Average）对成交量分布进行平滑，去除噪音
    ///
    /// 算法：对每个价格点，计算其周围 window_size 个点的平均值
    fn smooth_profile(
        &self,
        profile: &BTreeMap<Price, f32>,
        window_size: usize,
    ) -> BTreeMap<Price, f32> {
        if window_size <= 1 || profile.is_empty() {
            return profile.clone();
        }

        // 确保窗口大小为奇数
        let window_size = if window_size % 2 == 0 {
            window_size + 1
        } else {
            window_size
        };

        let prices: Vec<Price> = profile.keys().copied().collect();
        let mut smoothed = BTreeMap::new();

        let half_window = window_size / 2;

        for (idx, &price) in prices.iter().enumerate() {
            // 计算窗口范围
            let start_idx = idx.saturating_sub(half_window);
            let end_idx = (idx + half_window + 1).min(prices.len());

            // 计算窗口内成交量的平均值
            let window_prices = &prices[start_idx..end_idx];
            let sum: f32 = window_prices
                .iter()
                .filter_map(|p| profile.get(p))
                .sum();

            let count = window_prices.len();
            smoothed.insert(price, sum / count as f32);
        }

        smoothed
    }

    /// 第三步：峰值识别
    /// 寻找局部极大值，并应用阈值和宽度过滤
    ///
    /// 算法：
    /// 1. 遍历平滑后的成交量数组
    /// 2. 对于每个点，检查是否是局部极大值（V_i > V_{i-1} AND V_i > V_{i+1}）
    /// 3. 检查是否超过绝对阈值
    /// 4. 计算峰值宽度
    /// 5. 检查宽度是否满足最小要求
    fn detect_peaks(
        &self,
        smoothed_profile: &BTreeMap<Price, f32>,
        absolute_threshold: f32,
        min_peak_width: usize,
    ) -> Vec<HighVolumeNode> {
        if smoothed_profile.len() < 3 {
            return vec![];
        }

        let prices: Vec<Price> = smoothed_profile.keys().copied().collect();
        let volumes: Vec<f32> = prices
            .iter()
            .filter_map(|p| smoothed_profile.get(p))
            .copied()
            .collect();

        // 计算全局最大值（POC）
        let poc_volume = volumes.iter().copied().fold(0.0, f32::max);

        if poc_volume == 0.0 {
            return vec![];
        }

        let mut peaks = Vec::new();
        let mut i = 1;
        
        let mut local_maxima_count = 0;
        let mut above_threshold_count = 0;
        let mut valid_width_count = 0;
        let mut max_volume_seen = 0.0;
        let mut max_volume_idx = 0;
        
        while i < volumes.len() - 1 {
            let current_vol = volumes[i];
            let prev_vol = volumes[i - 1];
            let next_vol = volumes[i + 1];
            
            // 跟踪最大值
            if current_vol > max_volume_seen {
                max_volume_seen = current_vol;
                max_volume_idx = i;
            }

            // 检查是否是局部极大值
            // 条件：V_i > V_{i-1} AND V_i > V_{i+1}
            // 或者：V_i >= V_{i-1} AND V_i > V_{i+1}（允许相等，处理平台）
            // 或者：V_i > V_{i-1} AND V_i >= V_{i+1}（允许相等，处理平台）
            let is_local_max = (current_vol > prev_vol && current_vol > next_vol) ||
                               (current_vol >= prev_vol && current_vol > next_vol && i > 1) ||
                               (current_vol > prev_vol && current_vol >= next_vol && i < volumes.len() - 2);
            
            if is_local_max {
                local_maxima_count += 1;
                
                // 检查是否超过阈值
                if current_vol >= absolute_threshold {
                    above_threshold_count += 1;
                    
                    // 计算峰值宽度
                    let peak_width = self.calculate_peak_width(
                        &prices,
                        &volumes,
                        i,
                        current_vol,
                    );

                    // 检查宽度是否满足要求
                    if peak_width >= min_peak_width {
                        valid_width_count += 1;
                        let peak_price = prices[i];
                        let strength = current_vol / poc_volume;

                        peaks.push(HighVolumeNode {
                            price: peak_price,
                            volume: current_vol,
                            width: peak_width,
                            strength,
                        });
                    }
                }
            }

            i += 1;
        }

        // 按价格排序
        peaks.sort_by(|a, b| a.price.cmp(&b.price));

        peaks
    }

    /// 计算峰值宽度
    /// 从峰值点向两侧扩展，直到成交量下降到峰值的一定比例
    ///
    /// 算法：
    /// 1. 从峰值点向左扩展，直到成交量 < 峰值 * threshold_ratio
    /// 2. 从峰值点向右扩展，直到成交量 < 峰值 * threshold_ratio
    /// 3. 返回左右边界之间的宽度
    fn calculate_peak_width(
        &self,
        prices: &[Price],
        volumes: &[f32],
        peak_idx: usize,
        peak_volume: f32,
    ) -> usize {
        // 使用 50% 阈值来确定峰值边界
        let threshold = peak_volume * 0.5;

        // 向左扩展
        let mut left = peak_idx;
        while left > 0 && volumes[left - 1] >= threshold {
            left -= 1;
        }

        // 向右扩展
        let mut right = peak_idx;
        while right < volumes.len() - 1 && volumes[right + 1] >= threshold {
            right += 1;
        }

        right - left + 1
    }
}
