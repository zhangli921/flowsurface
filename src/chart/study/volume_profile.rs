use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use iced::{widget::canvas::{self, Cache}, Point, Rectangle, Size, Theme};
use exchange::util::Price;

use crate::chart::{PlotData, ViewState};
use data::chart::kline::KlineDataPoint;
use data::aggr::ticks::TickAccumulation; // Added for Error 3

const RECALC_INTERVAL: Duration = Duration::from_millis(200);
const SAMPLING_THRESHOLD: usize = 500;

/// Holds the calculated data for a volume profile.
#[derive(Clone, Debug)] // Removed Default
pub struct VolumeProfile {
    /// Maps price levels to the total volume traded at that level.
    pub profile: BTreeMap<Price, f32>,
    /// The price level with the highest volume.
    pub poc_price: Price,
    /// The highest volume found in the profile.
    pub max_volume: f32,
}

/// Manages the state, calculation, and rendering of the Volume Profile overlay.
#[derive(Debug)]
pub struct VolumeProfileStudy {
    pub is_enabled: bool,
    pub cache: Cache,
    profile_data: Option<VolumeProfile>,
    last_recalc_time: Instant,
}

impl VolumeProfileStudy {
    pub fn new() -> Self {
        Self {
            is_enabled: false,
            cache: Cache::new(),
            profile_data: None,
            last_recalc_time: Instant::now(),
        }
    }

    pub fn new_with_config(cfg: data::chart::kline::Config) -> Self {
        Self {
            is_enabled: cfg.volume_profile_enabled,
            cache: Cache::new(),
            profile_data: None,
            last_recalc_time: Instant::now(),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.is_enabled = enabled;
        if !enabled {
            self.profile_data = None;
        }
    }

    /// Updates the volume profile data if needed (based on throttling).
    pub fn update(
        &mut self,
        now: Instant,
        chart_state: &ViewState,
        data_source: &PlotData<KlineDataPoint>,
    ) {
        if !self.is_enabled {
            return;
        }

        if now.duration_since(self.last_recalc_time) > RECALC_INTERVAL {
            self.last_recalc_time = now;
            self.recalculate(chart_state, data_source);
        }
    }

    /// Performs the actual calculation of the volume profile.
    fn recalculate(&mut self, chart_state: &ViewState, data_source: &PlotData<KlineDataPoint>) {
        let visible_region = chart_state.visible_region(chart_state.bounds.size());
        let (start_interval, end_interval) = chart_state.interval_range(&visible_region);

        if end_interval < start_interval {
            self.profile_data = None;
            return;
        }

        let mut profile_map: BTreeMap<Price, f32> = BTreeMap::new();

        match data_source {
            PlotData::TimeBased(pyramid) => {
                if let Some((_, level_0)) = pyramid.levels.first() {
                    let range = level_0.range(start_interval..=end_interval);
                    let count = range.clone().count();

                    let mut process_dp = |dp: &KlineDataPoint| {
                        for (price, trade_group) in &dp.footprint.trades {
                            *profile_map.entry(*price).or_insert(0.0) += trade_group.total_qty();
                        }
                    };

                    if count > SAMPLING_THRESHOLD {
                        let step = (count / SAMPLING_THRESHOLD).max(1);
                        range.step_by(step).for_each(|(_, dp)| process_dp(dp));
                    } else {
                        range.for_each(|(_, dp)| process_dp(dp));
                    }
                }
            }
            PlotData::TickBased(tick_aggr) => {
                let start_idx = start_interval as usize;
                let end_idx = end_interval as usize;

                if let Some(slice) = tick_aggr.datapoints.get(start_idx..=end_idx) {
                    let count = slice.len();

                    let mut process_tick_acc = |tick_acc: &TickAccumulation| {
                        for (price, trade_group) in &tick_acc.footprint.trades {
                            *profile_map.entry(*price).or_insert(0.0) += trade_group.total_qty();
                        }
                    };

                    if count > SAMPLING_THRESHOLD {
                        let step = (count / SAMPLING_THRESHOLD).max(1);
                        slice.iter().step_by(step).for_each(process_tick_acc);
                    } else {
                        slice.iter().for_each(process_tick_acc);
                    }
                }
            }
        }

        if profile_map.is_empty() {
            self.profile_data = None;
            return;
        }

        // Find the Point of Control (POC) and max volume
        let mut profile_iter = profile_map.iter();
        let (&initial_price, &initial_volume) = profile_iter.next().unwrap(); // Safe because profile_map is not empty

        let (poc_price, max_volume) = profile_iter.fold(
            (initial_price, initial_volume),
            |(current_poc_price, max_vol), (&price, &vol)| {
                if vol > max_vol {
                    (price, vol)
                } else {
                    (current_poc_price, max_vol)
                }
            },
        );

        self.profile_data = Some(VolumeProfile {
            profile: profile_map,
            poc_price,
            max_volume,
        });
    }

    /// Draws the volume profile on the canvas.
    pub fn draw(
        &self,
        frame: &mut canvas::Frame,
        bounds: Rectangle,
        chart_state: &ViewState,
        theme: &Theme,
    ) {
        let Some(profile) = self.profile_data.as_ref() else {
            return;
        };

        if !self.is_enabled || profile.max_volume <= 0.0 {
            return;
        }

        let palette = theme.extended_palette();
        let poc_color = palette.warning.weak.color;
        let bar_color = palette.background.strong.color;

        // Use 30% of the chart width for the volume profile
        let max_draw_width = bounds.width * 0.3;
        let bar_height = chart_state.cell_height; // 原始的 bar_height

        // --- 新增的价格区间合并逻辑 ---
        let mut profiles_to_draw: BTreeMap<i64, (f32, Price)> = BTreeMap::new(); // key: pixel_row, value: (total_volume, poc_price_in_bin)
        let merge_threshold = 1.0; // 1像素，如果cell_height小于1像素，则进行合并

        if bar_height < merge_threshold {
            // 需要合并价格区间
            for (&price, &volume) in &profile.profile {
                // 计算当前价格在屏幕上的像素行
                let y_pixel = chart_state.price_to_y(price);
                let pixel_row = y_pixel.round() as i64;

                profiles_to_draw
                    .entry(pixel_row)
                    .and_modify(|(total_vol, _)| *total_vol += volume)
                    .or_insert((volume, price)); // 存储第一个遇到的价格作为该bin的代表价格
            }
        } else {
            // 不需要合并，直接使用原始数据
            for (&price, &volume) in &profile.profile {
                profiles_to_draw.insert(chart_state.price_to_y(price).round() as i64, (volume, price));
            }
        }
        // --- 价格区间合并逻辑结束 ---

        for (&pixel_row, &(volume, price_in_bin)) in &profiles_to_draw {
            // Don't draw bars that are outside the visible price range
            // 注意：这里需要根据合并后的价格来判断，或者直接使用pixel_row的范围
            // 为了简化，我们假设合并后的价格仍在可见范围内
            // 实际绘制时，y坐标直接使用pixel_row
            let y = pixel_row as f32; // 直接使用像素行作为y坐标

            let bar_length = (volume / profile.max_volume) * max_draw_width;
            if bar_length < 1.0 {
                continue;
            }

            let color = if price_in_bin == profile.poc_price { // 使用bin内的代表价格判断是否是POC
                poc_color
            } else {
                bar_color
            };

            // 绘制时，高度使用合并后的像素高度，即1像素
            frame.fill_rectangle(
                Point::new(bounds.width - bar_length, y - 0.5), // 高度为1像素，所以减去0.5
                Size::new(bar_length, 1.0), // 合并后高度固定为1像素
                color,
            );
        }
    }
}