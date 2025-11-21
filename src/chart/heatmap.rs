use super::{
    Chart, Message, PlotConstants, ViewState,
};
use crate::chart::renderer;
use crate::modal::pane::settings::study::{self, Study};
use data::chart::{
    Basis, ViewConfig,
    heatmap::{
        CLEANUP_THRESHOLD, Config, HeatmapDataPoint, HeatmapStudy, HistoricalDepth, ProfileKind,
        QtyScale,
    },
    indicator::HeatmapIndicator,
};
use data::util::{count_decimals};
use data::{
    aggr::time::{DataPoint, TimeSeries},
    chart::Autoscale,
};
use exchange::util::{Price, PriceStep};
use exchange::{TickerInfo, Trade, depth::Depth};

use iced::{
    Element, Vector,
};

use enum_map::EnumMap;
use std::time::Instant;

const MIN_SCALING: f32 = 0.6;
const MAX_SCALING: f32 = 1.2;

const MAX_CELL_WIDTH: f32 = 12.0;
const MIN_CELL_WIDTH: f32 = 1.0;

const MAX_CELL_HEIGHT: f32 = 10.0;
const MIN_CELL_HEIGHT: f32 = 1.0;

const DEFAULT_CELL_WIDTH: f32 = 3.0;

const TOOLTIP_WIDTH: f32 = 198.0;
const TOOLTIP_HEIGHT: f32 = 66.0;
const TOOLTIP_PADDING: f32 = 12.0;

const MAX_CIRCLE_RADIUS: f32 = 16.0;

impl Chart for HeatmapChart {
    type IndicatorKind = HeatmapIndicator;

    fn state(&self) -> &super::ChartState {
        &self.chart.state
    }

    fn mut_state(&mut self) -> &mut super::ChartState {
        &mut self.chart.state
    }

    fn chart_data(&self) -> renderer::ChartData {
        // TODO: This is a placeholder. Heatmap data needs to be properly
        // converted into a format the new renderer can use.
        renderer::ChartData {
            kline_data: vec![],
            svp_data: vec![],
            view_state: self.chart.clone(),
        }
    }

    fn invalidate_crosshair(&mut self) {
        // self.chart.cache.clear_crosshair();
    }

    fn invalidate_all(&mut self) {
        self.invalidate(None);
    }

    fn view_indicators(&'_ self, _indicators: &[Self::IndicatorKind]) -> Vec<Element<'_, Message>> {
        vec![]
    }

    fn visible_timerange(&self) -> Option<(u64, u64)> {
        let chart = self.state();
        let region = chart.visible_region(chart.bounds.size());

        if region.width == 0.0 {
            return None;
        }

        Some((
            chart.x_to_interval(region.x),
            chart.x_to_interval(region.x + region.width),
        ))
    }

    fn interval_keys(&self) -> Option<Vec<u64>> {
        None
    }

    fn autoscaled_coords(&self) -> Vector {
        let chart = self.state();
        Vector::new(
            0.5 * (chart.bounds.width / chart.scaling) - (90.0 / chart.scaling),
            chart.translation.y,
        )
    }

    fn supports_fit_autoscaling(&self) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        self.trades.datapoints.is_empty()
    }
}

impl PlotConstants for HeatmapChart {
    fn min_scaling(&self) -> f32 {
        MIN_SCALING
    }

    fn max_scaling(&self) -> f32 {
        MAX_SCALING
    }

    fn max_cell_width(&self) -> f32 {
        MAX_CELL_WIDTH
    }

    fn min_cell_width(&self) -> f32 {
        MIN_CELL_WIDTH
    }

    fn max_cell_height(&self) -> f32 {
        MAX_CELL_HEIGHT
    }

    fn min_cell_height(&self) -> f32 {
        MIN_CELL_HEIGHT
    }

    fn default_cell_width(&self) -> f32 {
        DEFAULT_CELL_WIDTH
    }
}

#[derive(Default)]
enum IndicatorData {
    #[default]
    Volume,
}

pub struct HeatmapChart {
    chart: ViewState,
    trades: TimeSeries<HeatmapDataPoint>,
    indicators: EnumMap<HeatmapIndicator, Option<IndicatorData>>,
    pause_buffer: Vec<(u64, Box<[Trade]>, Depth)>,
    heatmap: HistoricalDepth,
    visual_config: Config,
    study_configurator: study::Configurator<HeatmapStudy>,
    last_tick: Instant,
    pub studies: Vec<HeatmapStudy>,
}

impl HeatmapChart {
    pub fn new(
        layout: ViewConfig,
        basis: Basis,
        tick_size: f32,
        enabled_indicators: &[HeatmapIndicator],
        ticker_info: TickerInfo,
        config: Option<Config>,
        studies: Vec<HeatmapStudy>,
    ) -> Self {
        let step = PriceStep::from_f32(tick_size);

        let mut indicators = EnumMap::default();
        for &indicator in enabled_indicators {
            indicators[indicator] = Some(match indicator {
                HeatmapIndicator::Volume => IndicatorData::Volume,
            });
        }

        let heatmap = HistoricalDepth::new(ticker_info.min_qty.into(), step, basis);

        let view_state = ViewState::new(
            basis,
            step,
            count_decimals(tick_size),
            ticker_info,
            ViewConfig {
                splits: layout.splits,
                autoscale: Some(Autoscale::CenterLatest),
            },
            DEFAULT_CELL_WIDTH,
            4.0,
        );

        HeatmapChart {
            chart: view_state,
            indicators,
            pause_buffer: vec![],
            heatmap,
            trades: TimeSeries::<HeatmapDataPoint>::new(basis, step),
            visual_config: config.unwrap_or_default(),
            study_configurator: study::Configurator::new(),
            studies,
            last_tick: Instant::now(),
        }
    }

    pub fn insert_datapoint(
        &mut self,
        trades_buffer: &[Trade],
        depth_update_t: u64,
        depth: &Depth,
    ) {
        let chart = &mut self.chart.state;

        let mid_price = depth.mid_price().unwrap_or(chart.base_price_y);
        // chart.last_price = Some(PriceInfoLabel::Neutral(mid_price));

        // if current orderbook not visible, pause the data insertion and buffer them instead
        let is_paused = { chart.translation.x * chart.scaling > chart.bounds.width / 2.0 };

        if is_paused {
            self.pause_buffer.push((
                depth_update_t,
                trades_buffer.to_vec().into_boxed_slice(),
                depth.clone(),
            ));

            return;
        } else if !self.pause_buffer.is_empty() {
            self.pause_buffer.sort_by_key(|(time, _, _)| *time);

            for (time, trades, depth) in std::mem::take(&mut self.pause_buffer) {
                self.process_datapoint(&trades, time, &depth);
            }
        } else {
            self.cleanup_old_data();
        }

        self.process_datapoint(trades_buffer, depth_update_t, depth);
    }

    fn cleanup_old_data(&mut self) {
        if self.trades.datapoints.len() > CLEANUP_THRESHOLD {
            let keys_to_remove = self
                .trades
                .datapoints
                .keys()
                .take(CLEANUP_THRESHOLD / 10)
                .copied()
                .collect::<Vec<u64>>();

            for key in keys_to_remove {
                self.trades.datapoints.remove(&key);
            }

            if let Some(oldest_time) = self.trades.datapoints.keys().next().copied() {
                self.heatmap.cleanup_old_price_levels(oldest_time);
            }
        }
    }

    fn process_datapoint(&mut self, trades_buffer: &[Trade], depth_update: u64, depth: &Depth) {
        let chart = &mut self.chart.state;

        let aggregate_time: u64 = match chart.basis {
            Basis::Time(interval) => interval.into(),
            Basis::Tick(_) => todo!(),
        };

        let rounded_depth_update = (depth_update / aggregate_time) * aggregate_time;

        {
            let entry = self
                .trades
                .datapoints
                .entry(rounded_depth_update)
                .or_insert_with(|| HeatmapDataPoint {
                    grouped_trades: Box::new([]),
                    buy_sell: (0.0, 0.0),
                });

            for trade in trades_buffer {
                entry.add_trade(trade, chart.tick_size);
            }
        }

        self.heatmap
            .insert_latest_depth(depth, rounded_depth_update);

        {
            let mid_price = depth.mid_price().unwrap_or(chart.base_price_y);
            chart.base_price_y = mid_price.round_to_step(chart.tick_size);
        }

        chart.latest_x = rounded_depth_update;
    }

    pub fn visual_config(&self) -> Config {
        self.visual_config
    }

    pub fn set_visual_config(&mut self, visual_config: Config) {
        self.visual_config = visual_config;
        self.invalidate(Some(Instant::now()));
    }

    pub fn set_basis(&mut self, basis: Basis) {
        self.chart.state.basis = basis;

        self.trades.datapoints.clear();
        self.heatmap = HistoricalDepth::new(
            self.chart.state.ticker_info.min_qty.into(),
            self.chart.state.tick_size,
            basis,
        );

        let chart = &mut self.chart.state;
        chart.translation = Vector::new(
            0.5 * (chart.bounds.width / chart.scaling) - (90.0 / chart.scaling),
            0.0,
        );

        self.invalidate(None);
    }

    pub fn study_configurator(&self) -> &study::Configurator<HeatmapStudy> {
        &self.study_configurator
    }

    pub fn update_study_configurator(&mut self, message: study::Message<HeatmapStudy>) {
        let studies = &mut self.studies;

        match self.study_configurator.update(message) {
            Some(study::Action::ToggleStudy(study, is_selected)) => {
                if is_selected {
                    let already_exists = studies.iter().any(|s| s.is_same_type(&study));
                    if !already_exists {
                        studies.push(study);
                    }
                } else {
                    studies.retain(|s| !s.is_same_type(&study));
                }
            }
            Some(study::Action::ConfigureStudy(study)) => {
                if let Some(existing_study) = studies.iter_mut().find(|s| s.is_same_type(&study)) {
                    *existing_study = study;
                }
            }
            None => {}
        }

        self.invalidate(None);
    }

    pub fn basis_interval(&self) -> Option<u64> {
        match self.chart.state.basis {
            Basis::Time(interval) => Some(interval.into()),
            Basis::Tick(_) => None,
        }
    }

    pub fn chart_layout(&self) -> ViewConfig {
        self.chart.state.layout.clone()
    }

    pub fn change_tick_size(&mut self, new_tick_size: f32) {
        let chart_state = self.mut_state();

        let basis = chart_state.basis;
        let step = PriceStep::from_f32(new_tick_size);

        chart_state.cell_height = 4.0;
        chart_state.tick_size = step;
        chart_state.decimals = count_decimals(new_tick_size);

        self.trades.datapoints.clear();
        self.heatmap = HistoricalDepth::new(self.chart.state.ticker_info.min_qty.into(), step, basis);
    }

    pub fn tick_size(&self) -> f32 {
        self.chart.state.tick_size.to_f32_lossy()
    }

    pub fn toggle_indicator(&mut self, indicator: HeatmapIndicator) {
        if self.indicators[indicator].is_some() {
            self.indicators[indicator] = None;
        } else {
            let data = match indicator {
                HeatmapIndicator::Volume => IndicatorData::Volume,
            };
            self.indicators[indicator] = Some(data);
        }
    }

    pub fn invalidate(&mut self, now: Option<Instant>) -> Option<super::Action> {
        let chart = &mut self.chart.state;

        if chart.layout.autoscale.is_some() {
            chart.translation = Vector::new(
                0.5 * (chart.bounds.width / chart.scaling) - (90.0 / chart.scaling),
                0.0,
            );
        }

        // self.chart.cache.clear_all();

        if let Some(t) = now {
            self.last_tick = t;
        }

        None
    }

    pub fn last_update(&self) -> Instant {
        self.last_tick
    }

    fn calc_qty_scales(
        &self,
        earliest: u64,
        latest: u64,
        highest: Price,
        lowest: Price,
    ) -> QtyScale {
        let market_type = self.chart.state.ticker_info.market_type();

        let (max_trade_qty, max_aggr_volume) =
            self.trades.max_trade_qty_and_aggr_volume(earliest, latest);

        let max_depth_qty = self.heatmap.max_depth_qty_in_range(
            earliest,
            latest,
            highest,
            lowest,
            market_type,
            self.visual_config.order_size_filter,
        );

        QtyScale {
            max_trade_qty,
            max_aggr_volume,
            max_depth_qty,
        }
    }
}

