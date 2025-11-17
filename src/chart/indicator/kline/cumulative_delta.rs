use crate::chart::{
    Caches, Message, ViewState,
    indicator::{
        indicator_row,
        kline::KlineIndicatorImpl,
        plot::{
            PlotTooltip,
            line::LinePlot,
        },
    },
};

use data::chart::{PlotData, kline::KlineDataPoint};
use data::util::format_with_commas;
use exchange::{Kline, Trade};

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

pub struct CumulativeDeltaIndicator {
    cache: Caches,
    data: BTreeMap<u64, f32>,
}

impl CumulativeDeltaIndicator {
    pub fn new() -> Self {
        Self {
            cache: Caches::default(),
            data: BTreeMap::new(),
        }
    }

    fn indicator_elem<'a>(
        &'a self,
        main_chart: &'a ViewState,
        visible_range: RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        let tooltip = |&value: &f32, _next: Option<&f32>| {
            PlotTooltip::new(format!("Cumulative Delta: {}", format_with_commas(value)))
        };

        let plot = LinePlot::new(|&v: &f32| v)
            .with_tooltip(tooltip);

        indicator_row(main_chart, &self.cache, plot, &self.data, visible_range)
    }
}

impl KlineIndicatorImpl for CumulativeDeltaIndicator {
    fn clear_all_caches(&mut self) {
        self.cache.clear_all();
    }

    fn clear_crosshair_caches(&mut self) {
        self.cache.clear_crosshair();
    }

    fn element<'a>(
        &'a self,
        chart: &'a ViewState,
        visible_range: RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        self.indicator_elem(chart, visible_range)
    }

    fn rebuild_from_source(&mut self, source: &PlotData<KlineDataPoint>) {
        let volume_data = match source {
            PlotData::TimeBased(timeseries) => timeseries.volume_data(),
            PlotData::TickBased(tickseries) => tickseries.volume_data(),
        };

        let mut cumulative_delta = 0.0;
        let mut new_data = BTreeMap::new();

        for (time, (buy_vol, sell_vol)) in volume_data {
            let delta = if buy_vol == -1.0 { // bybit workaround
                sell_vol
            } else {
                buy_vol - sell_vol
            };
            cumulative_delta += delta;
            new_data.insert(time, cumulative_delta);
        }

        self.data = new_data;
        self.clear_all_caches();
    }

    fn on_insert_klines(&mut self, klines: &[Kline]) {
        let mut last_cumulative_delta = self.data.values().last().copied().unwrap_or(0.0);

        for kline in klines {
            let delta = if kline.volume.0 == -1.0 { // bybit workaround
                kline.volume.1
            } else {
                kline.volume.0 - kline.volume.1
            };
            last_cumulative_delta += delta;
            self.data.insert(kline.time, last_cumulative_delta);
        }
        self.clear_all_caches();
    }

    fn on_insert_trades(
        &mut self,
        _trades: &[Trade],
        _old_dp_len: usize,
        source: &PlotData<KlineDataPoint>,
    ) {
        // For tick-based charts, we need to recalculate from the affected point.
        // This is simpler to just rebuild for now. A more optimized version could be implemented later.
        if let PlotData::TickBased(_) = source {
            self.rebuild_from_source(source);
        }
    }

    fn on_ticksize_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.rebuild_from_source(source);
    }

    fn on_basis_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.rebuild_from_source(source);
    }
}
