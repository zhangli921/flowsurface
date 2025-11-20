use std::collections::BTreeMap;

use crate::chart::Basis;
use crate::chart::heatmap::HeatmapDataPoint;
use crate::chart::kline::{ClusterKind, KlineDataPoint, KlineTrades, NPoc};

use exchange::util::{Price, PriceStep};
use exchange::{Kline, Timeframe, Trade};

pub trait DataPoint {
    fn add_trade(&mut self, trade: &Trade, step: PriceStep);

    fn clear_trades(&mut self);

    fn merge(&mut self, other: &Self);

    fn last_trade_time(&self) -> Option<u64>;

    fn first_trade_time(&self) -> Option<u64>;

    fn last_price(&self) -> Price;

    fn kline(&self) -> Option<&Kline>;

    fn value_high(&self) -> Price;

    fn value_low(&self) -> Price;
}

pub struct DataPyramid<D: DataPoint> {
    pub levels: Vec<(Timeframe, BTreeMap<u64, D>)>,
    pub base_interval: Timeframe,
    pub tick_size: PriceStep,
}

impl<D: DataPoint + Clone> DataPyramid<D> {
    pub fn base_price(&self) -> Price {
        self.levels.first()
            .and_then(|(_, datapoints)| datapoints.values().last())
            .map_or(Price::from_f32(0.0), DataPoint::last_price)
    }

    pub fn latest_timestamp(&self) -> Option<u64> {
        self.levels.first()
            .and_then(|(_, datapoints)| datapoints.keys().last().copied())
    }

    pub fn latest_kline(&self) -> Option<&Kline> {
        self.levels.first()
            .and_then(|(_, datapoints)| datapoints.values().last())
            .and_then(|dp| dp.kline())
    }

    pub fn price_scale(&self, lookback: usize) -> (Price, Price) {
        let mut iter = self.levels.first()
            .and_then(|(_, datapoints)| Some(datapoints.iter().rev().take(lookback)))
            .unwrap();

        if let Some((_, first)) = iter.next() {
            let mut high = first.value_high();
            let mut low = first.value_low();

            for (_, dp) in iter {
                let value_high = dp.value_high();
                let value_low = dp.value_low();
                if value_high > high {
                    high = value_high;
                }
                if value_low < low {
                    low = value_low;
                }
            }

            (high, low)
        } else {
            (Price::from_f32(0.0), Price::from_f32(0.0))
        }
    }

    pub fn volume_data<'a>(&'a self) -> BTreeMap<u64, (f32, f32)>
    where
        BTreeMap<u64, (f32, f32)>: From<&'a DataPyramid<D>>,
    {
        self.into()
    }

    pub fn timerange(&self) -> (u64, u64) {
        let earliest = self.levels.first()
            .and_then(|(_, datapoints)| datapoints.keys().next().copied())
            .unwrap_or(0);
        let latest = self.levels.first()
            .and_then(|(_, datapoints)| datapoints.keys().last().copied())
            .unwrap_or(0);

        (earliest, latest)
    }

    pub fn min_max_price_in_range_prices(
        &self,
        earliest: u64,
        latest: u64,
    ) -> Option<(Price, Price)> {
        let mut it = self.levels.first()?
            .1.range(earliest..=latest);

        let (_, first) = it.next()?;
        let mut min_price = first.value_low();
        let mut max_price = first.value_high();

        for (_, dp) in it {
            let low = dp.value_low();
            let high = dp.value_high();
            if low < min_price {
                min_price = low;
            }
            if high > max_price {
                max_price = high;
            }
        }

        Some((min_price, max_price))
    }

    pub fn min_max_price_in_range(&self, earliest: u64, latest: u64) -> Option<(f32, f32)> {
        self.min_max_price_in_range_prices(earliest, latest)
            .map(|(min_p, max_p)| (min_p.to_f32(), max_p.to_f32()))
    }

    pub fn clear_trades(&mut self) {
        for (_, datapoints) in self.levels.iter_mut() {
            for data_point in datapoints.values_mut() {
                data_point.clear_trades();
            }
        }
    }

    pub fn check_kline_integrity(
        &self,
        earliest: u64,
        latest: u64,
        interval: u64,
    ) -> Option<Vec<u64>> {
        let datapoints = &self.levels.first()?.1;
        let mut time = earliest;
        let mut missing_count = 0;

        while time < latest {
            if !datapoints.contains_key(&time) {
                missing_count += 1;
                break;
            }
            time += interval;
        }

        if missing_count > 0 {
            let mut missing_keys = Vec::with_capacity(((latest - earliest) / interval) as usize);
            let mut time = earliest;

            while time < latest {
                if !datapoints.contains_key(&time) {
                    missing_keys.push(time);
                }
                time += interval;
            }

            log::warn!(
                "Integrity check failed: missing {} klines",
                missing_keys.len()
            );
            return Some(missing_keys);
        }

        None
    }

    pub fn select_level(&self, visible_bars: u64) -> &BTreeMap<u64, D> {
        // Heuristic: target around 100-200 bars on screen for good performance/detail balance
        // Adjust these numbers based on testing
        const TARGET_MIN_BARS: u64 = 100;
        const TARGET_MAX_BARS: u64 = 200;

        let base_level_len = self.levels[0].1.len() as u64;
        if base_level_len == 0 {
            return &self.levels[0].1; // Return empty map if base is empty
        }

        // Determine the ideal level based on visible_bars
        let mut best_level_index = 0;
        for i in (0..self.levels.len()).rev() { // Iterate from coarsest to finest
            let (timeframe, _) = &self.levels[i];
            let level_interval_ms = timeframe.to_milliseconds();
            let base_interval_ms = self.base_interval.to_milliseconds();
            let aggregation_factor = level_interval_ms / base_interval_ms;

            // This calculation needs to be more robust, considering actual data density
            // For now, a simple ratio approximation
            let estimated_bars_at_this_level = visible_bars / aggregation_factor;

            if estimated_bars_at_this_level >= TARGET_MIN_BARS || i == 0 {
                best_level_index = i;
                break;
            }
        }
        
        &self.levels[best_level_index].1
    }
}

impl DataPyramid<KlineDataPoint> {
    pub fn new(interval: Timeframe, tick_size: PriceStep, klines: &[Kline]) -> Self {
        let mut pyramid = Self {
            levels: vec![(interval, BTreeMap::new())],
            base_interval: interval,
            tick_size,
        };

        pyramid.insert_klines(klines);
        pyramid
    }

    pub fn build_pyramid(&mut self) {
        // TODO: Define level definitions
        // For now, let's just create one extra level for demonstration (e.g., 5x base_interval)
        let aggregation_factor = 5;
        if let Ok(higher_interval) = Timeframe::from_milliseconds(self.base_interval.to_milliseconds() * aggregation_factor) {
        
            if self.levels.len() > 1 { // Already built
                return;
            }

            let level_0 = &self.levels[0].1;
            if level_0.is_empty() {
                return;
            }

            let mut level_1 = BTreeMap::new();
            let mut current_agg: Option<KlineDataPoint> = None;
            let mut count = 0;

            for (_, dp) in level_0.iter() {
                if let Some(agg) = &mut current_agg {
                    agg.merge(dp);
                    count += 1;
                    if count >= aggregation_factor {
                        level_1.insert(agg.kline.time, current_agg.take().unwrap());
                        count = 0;
                    }
                } else {
                    current_agg = Some(dp.clone());
                    count = 1;
                }
            }
            // Insert any remaining aggregated data
            if let Some(agg) = current_agg {
                if count > 0 {
                    level_1.insert(agg.kline.time, agg);
                }
            }

            if !level_1.is_empty() {
                self.levels.push((higher_interval, level_1));
            }
        }
    }

    pub fn with_trades(&self, trades: &[Trade]) -> DataPyramid<KlineDataPoint> {
        let mut new_pyramid = Self {
            levels: self.levels.clone(),
            base_interval: self.base_interval,
            tick_size: self.tick_size,
        };

        new_pyramid.insert_trades_or_create_bucket(trades);
        new_pyramid
    }

    pub fn insert_klines(&mut self, klines: &[Kline]) {
        let level_0 = &mut self.levels[0].1;
        for kline in klines {
            let entry =
                level_0
                .entry(kline.time)
                .or_insert_with(|| KlineDataPoint {
                    kline: *kline,
                    footprint: KlineTrades::new(),
                });

            entry.kline = *kline;
        }

        self.update_poc_status();
        self.build_pyramid(); // Rebuild pyramid after inserting new klines
    }

    pub fn insert_trades_or_create_bucket(&mut self, buffer: &[Trade]) {
        if buffer.is_empty() {
            return;
        }
        let aggr_time = self.base_interval.to_milliseconds();
        let mut updated_times = Vec::new();

        let level_0 = &mut self.levels[0].1;

        buffer.iter().for_each(|trade| {
            let rounded_time = (trade.time / aggr_time) * aggr_time;

            if !updated_times.contains(&rounded_time) {
                updated_times.push(rounded_time);
            }

            let entry =
                level_0
                .entry(rounded_time)
                .or_insert_with(|| KlineDataPoint {
                    kline: Kline {
                        time: rounded_time,
                        open: trade.price,
                        high: trade.price,
                        low: trade.price,
                        close: trade.price,
                        volume: (0.0, 0.0),
                    },
                    footprint: KlineTrades::new(),
                });

            entry.add_trade(trade, self.tick_size);
        });

        for time in updated_times {
            if let Some(data_point) = level_0.get_mut(&time) {
                data_point.calculate_poc();
            }
        }
    }

    pub fn insert_trades_existing_buckets(&mut self, buffer: &[Trade]) {
        if buffer.is_empty() {
            return;
        }
        let aggr_time = self.base_interval.to_milliseconds();
        let mut updated_times: Vec<u64> = Vec::new();
        let level_0 = &mut self.levels[0].1;

        for trade in buffer {
            let rounded_time = (trade.time / aggr_time) * aggr_time;

            if let Some(entry) = level_0.get_mut(&rounded_time) {
                if !updated_times.contains(&rounded_time) {
                    updated_times.push(rounded_time);
                }
                entry.add_trade(trade, self.tick_size);
            }
        }

        for time in updated_times {
            if let Some(data_point) = level_0.get_mut(&time) {
                data_point.calculate_poc();
            }
        }
    }

    pub fn change_tick_size(&mut self, tick_size: f32, raw_trades: &[Trade]) {
        self.tick_size = PriceStep::from_f32(tick_size);
        self.clear_trades();

        if !raw_trades.is_empty() {
            self.insert_trades_existing_buckets(raw_trades);
        }
    }

    pub fn update_poc_status(&mut self) {
        let level_0 = &mut self.levels[0].1;
        let updates = level_0
            .iter()
            .filter_map(|(&time, dp)| dp.poc_price().map(|price| (time, price)))
            .collect::<Vec<_>>();

        for (current_time, poc_price) in updates {
            let mut npoc = NPoc::default();

            for (&next_time, next_dp) in level_0.range((current_time + 1)..) {
                let next_dp_low = next_dp.kline.low.round_to_side_step(true, self.tick_size);
                let next_dp_high = next_dp.kline.high.round_to_side_step(false, self.tick_size);

                if next_dp_low <= poc_price && next_dp_high >= poc_price {
                    npoc.filled(next_time);
                    break;
                } else {
                    npoc.unfilled();
                }
            }

            if let Some(data_point) = level_0.get_mut(&current_time) {
                data_point.set_poc_status(npoc);
            }
        }
    }

    pub fn suggest_trade_fetch_range(
        &self,
        visible_earliest: u64,
        visible_latest: u64,
    ) -> Option<(u64, u64)> {
        let datapoints = &self.levels.first()?.1;
        if datapoints.is_empty() {
            return None;
        }

        if let Some((last_t_before_gap, first_t_after_gap)) = self.find_trade_gap() {
            if last_t_before_gap.is_none() && first_t_after_gap.is_none() {
                // No trades at all, fetch for the visible range
                return Some((visible_earliest, visible_latest));
            }

            let (data_earliest, data_latest) = self.timerange();

            let fetch_from = last_t_before_gap
                .map_or(data_earliest, |t| t.saturating_add(1))
                .max(visible_earliest);
            let fetch_to = first_t_after_gap
                .map_or(data_latest, |t| t.saturating_sub(1))
                .min(visible_latest);

            if fetch_from < fetch_to {
                Some((fetch_from, fetch_to))
            } else {
                None
            }
        } else {
            // No gap found, all trades are present
            None
        }
    }

    fn find_trade_gap(&self) -> Option<(Option<u64>, Option<u64>)> {
        let datapoints = &self.levels.first()?.1;
        let empty_kline_time = datapoints
            .iter()
            .rev()
            .find(|(_, dp)| dp.footprint.trades.is_empty())
            .map(|(&time, _)| time);

        if let Some(target_time) = empty_kline_time {
            let last_t_before_gap = datapoints
                .range(..target_time)
                .rev()
                .find_map(|(_, dp)| dp.last_trade_time());

            let first_t_after_gap = datapoints
                .range(target_time + 1..)
                .find_map(|(_, dp)| dp.first_trade_time());

            Some((last_t_before_gap, first_t_after_gap))
        } else {
            None
        }
    }

    pub fn max_qty_ts_range(
        &self,
        cluster_kind: ClusterKind,
        earliest: u64,
        latest: u64,
        highest: Price,
        lowest: Price,
    ) -> f32 {
        let datapoints = &self.levels.first().unwrap().1;
        let mut max_cluster_qty: f32 = 0.0;

        datapoints
            .range(earliest..=latest)
            .for_each(|(_, dp)| {
                max_cluster_qty =
                    max_cluster_qty.max(dp.max_cluster_qty(cluster_kind, highest, lowest));
            });

        max_cluster_qty
    }
}

impl DataPyramid<HeatmapDataPoint> {
    pub fn new(basis: Basis, tick_size: PriceStep) -> Self {
        let timeframe = match basis {
            Basis::Time(interval) => interval,
            Basis::Tick(_) => unimplemented!(),
        };

        Self {
            levels: vec![(timeframe, BTreeMap::new())],
            base_interval: timeframe,
            tick_size,
        }
    }

    pub fn build_pyramid(&mut self) {
        let aggregation_factor = 5; // Example factor
        if let Ok(higher_interval) = Timeframe::from_milliseconds(self.base_interval.to_milliseconds() * aggregation_factor) {

            if self.levels.len() > 1 {
                return;
            }

            let level_0 = &self.levels[0].1;
            if level_0.is_empty() {
                return;
            }

            let mut level_1 = BTreeMap::new();
            let mut current_agg: Option<HeatmapDataPoint> = None;
            let mut count = 0;

            for (time, dp) in level_0.iter() {
                if let Some(agg) = &mut current_agg {
                    agg.merge(dp);
                    count += 1;
                    if count >= aggregation_factor {
                        level_1.insert(*time, current_agg.take().unwrap());
                        count = 0;
                    }
                } else {
                    current_agg = Some(dp.clone());
                    count = 1;
                }
            }
            // Insert any remaining aggregated data
            if let Some(agg) = current_agg {
                if count > 0 {
                    let last_dp_time = agg.last_trade_time().unwrap_or_else(|| {
                        // Fallback if no trade time
                        level_0.keys().next_back().copied().unwrap_or(0)
                    });
                    level_1.insert(last_dp_time, agg);
                }
            }

            if !level_1.is_empty() {
                self.levels.push((higher_interval, level_1));
            }
        }
    }

    pub fn max_trade_qty_and_aggr_volume(&self, earliest: u64, latest: u64) -> (f32, f32) {
        let datapoints = &self.levels.first().unwrap().1;
        let mut max_trade_qty = 0.0f32;
        let mut max_aggr_volume = 0.0f32;

        datapoints
            .range(earliest..=latest)
            .for_each(|(_, dp)| {
                let (mut buy_volume, mut sell_volume) = (0.0, 0.0);

                dp.grouped_trades.iter().for_each(|trade| {
                    max_trade_qty = max_trade_qty.max(trade.qty);

                    if trade.is_sell {
                        sell_volume += trade.qty;
                    } else {
                        buy_volume += trade.qty;
                    }
                });

                max_aggr_volume = max_aggr_volume.max(buy_volume + sell_volume);
            });

        (max_trade_qty, max_aggr_volume)
    }
}

impl From<&DataPyramid<KlineDataPoint>> for BTreeMap<u64, (f32, f32)> {
    /// Converts datapoints into a map of timestamps and volume data
    fn from(pyramid: &DataPyramid<KlineDataPoint>) -> Self {
        pyramid.levels.first().unwrap().1
            .iter()
            .map(|(time, dp)| (*time, (dp.kline.volume.0, dp.kline.volume.1)))
            .collect()
    }
}
