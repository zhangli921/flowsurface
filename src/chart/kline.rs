use crate::widget::vp_renderer::SessionVolumeProfile;
use super::{
    Action, Basis, Chart, ChartState, Message, PlotConstants, PlotData, ViewState,
    indicator,
};
use crate::chart::indicator::kline::KlineIndicatorImpl;
use crate::{modal::pane::settings::study};
use data::aggr::ticks::TickAggr;
use data::aggr::time::TimeSeries;
use data::chart::Autoscale;
use data::chart::kline::ClusterScaling;
use data::chart::{
    KlineChartKind, ViewConfig,
    indicator::{Indicator, KlineIndicator},
    kline::{ClusterKind, FootprintStudy, KlineDataPoint},
};
use data::util::{count_decimals};
use exchange::util::{Price, PriceStep};
use exchange::{
    Kline, OpenInterest as OIData, TickerInfo, Trade,
    fetcher::{RequestHandler},
};

use iced::task::Handle;
use std::sync::Arc;
use iced::{Element, Vector};

use enum_map::EnumMap;
use std::time::Instant;

use crate::chart::renderer;
use iced::widget::canvas::Cache;

impl Chart for KlineChart {
    type IndicatorKind = KlineIndicator;

    fn state(&self) -> &ChartState {
        &self.chart.state
    }

    fn mut_state(&mut self) -> &mut ChartState {
        &mut self.chart.state
    }

    fn chart_data(&self) -> renderer::ChartData {
        let kline_data = self.render_cache_kline.clone();
        
        // Only show VP if there are visible K-lines on screen
        // Use the same screen coordinate check method
        let svp_data = if self.has_visible_klines_on_screen() {
            self.chart.state.volume_profile.as_ref()
                .map(|vp| vp.bars.clone())
                .unwrap_or_else(|| Arc::new(Vec::new()))
        } else {
            // No visible K-lines - return empty VP data
            Arc::new(Vec::new())
        };
        
        renderer::ChartData {
            kline_data,
            svp_data,
            view_state: ViewState { state: self.chart.state.clone() },
        }
    }

    fn invalidate_all(&mut self) {
        self.invalidate(None);
    }

    fn invalidate_crosshair(&mut self) {
        // self.chart.cache.clear_crosshair();
        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.clear_crosshair_caches());
    }

    fn view_indicators(&'_ self, enabled: &[Self::IndicatorKind]) -> Vec<Element<'_, Message>> {
        let chart_state = self.state();
        // Use unified time range calculation (no padding for rendering)
        let (earliest, latest) = if let Some(range) = chart_state.visible_time_range_ms_for_render() {
            range
        } else {
            return vec![];
        };
        if earliest > latest {
            return vec![];
        }

        let market = chart_state.ticker_info.market_type();
        let mut elements = vec![];

        for selected_indicator in enabled {
            if !KlineIndicator::for_market(market).contains(selected_indicator) {
                continue;
            }
            if let Some(indi) = self.indicators[*selected_indicator].as_ref() {
                // TODO: Fix this to work with the new renderer
                // elements.push(indi.element(chart_state, earliest..=latest));
            }
        }
        elements
    }

    fn visible_timerange(&self) -> Option<(u64, u64)> {
        // Use unified time range calculation (no padding for rendering)
        self.state().visible_time_range_ms_for_render()
    }

    fn interval_keys(&self) -> Option<Vec<u64>> {
        match &self.data_source {
            PlotData::TimeBased(_) => None,
            PlotData::TickBased(tick_aggr) => Some(
                tick_aggr
                    .datapoints
                    .iter()
                    .map(|dp| dp.kline.time)
                    .collect(),
            ),
        }
    }

    fn autoscaled_coords(&self) -> Vector {
        let chart = self.state();
        let x_translation = match &self.kind {
            KlineChartKind::Footprint { .. } => {
                0.5 * (chart.bounds.width / chart.scaling) - (chart.cell_width / chart.scaling)
            }
            KlineChartKind::Candles => {
                0.5 * (chart.bounds.width / chart.scaling)
                    - (8.0 * chart.cell_width / chart.scaling)
            }
        };
        Vector::new(x_translation, chart.translation.y)
    }

    fn supports_fit_autoscaling(&self) -> bool {
        true
    }

    fn is_empty(&self) -> bool {
        match &self.data_source {
            PlotData::TimeBased(timeseries) => timeseries.datapoints.is_empty(),
            PlotData::TickBased(tick_aggr) => tick_aggr.datapoints.is_empty(),
        }
    }

    fn xaxis_cache(&self) -> &Cache {
        &self.xaxis_cache
    }

    fn yaxis_cache(&self) -> &Cache {
        &self.yaxis_cache
    }
    
    fn kline_data_for_price_range(&self) -> Option<&[data::kline::KLine]> {
        Some(&self.render_cache_kline)
    }
}

impl PlotConstants for KlineChart {
    fn min_scaling(&self) -> f32 {
        self.kind.min_scaling()
    }

    fn max_scaling(&self) -> f32 {
        self.kind.max_scaling()
    }

    fn max_cell_width(&self) -> f32 {
        self.kind.max_cell_width()
    }

    fn min_cell_width(&self) -> f32 {
        self.kind.min_cell_width()
    }

    fn max_cell_height(&self) -> f32 {
        self.kind.max_cell_height()
    }

    fn min_cell_height(&self) -> f32 {
        self.kind.min_cell_height()
    }

    fn default_cell_width(&self) -> f32 {
        self.kind.default_cell_width()
    }
}

pub struct KlineChart {
    chart: ViewState,
    data_source: PlotData<KlineDataPoint>,
    raw_trades: Vec<Trade>,
    indicators: EnumMap<KlineIndicator, Option<Box<dyn KlineIndicatorImpl>>>,
    fetching_trades: (bool, Option<Handle>),
    pub(crate) kind: KlineChartKind,
    request_handler: RequestHandler,
    study_configurator: study::Configurator<FootprintStudy>,
    last_tick: Instant,
    last_vp_request: Instant,
    vp_data: Option<SessionVolumeProfile>,
    xaxis_cache: Cache,
    yaxis_cache: Cache,
    render_cache_kline: Arc<Vec<data::kline::KLine>>,
    last_visible_range: Option<(u64, u64)>, // Track last visible time range for change detection
}

impl KlineChart {
    pub fn new(
        layout: ViewConfig,
        basis: Basis,
        tick_size: f32,
        klines_raw: &[Kline],
        raw_trades: Vec<Trade>,
        enabled_indicators: &[KlineIndicator],
        ticker_info: TickerInfo,
        kind: &KlineChartKind,
    ) -> Self {
        match basis {
            Basis::Time(interval) => {
                let step = PriceStep::from_f32(tick_size);

                let timeseries = TimeSeries::<KlineDataPoint>::new(interval, step, klines_raw)
                    .with_trades(&raw_trades);

                let base_price_y = timeseries.base_price();
                // Fallback to current time if no kline data, so VP can find data in test_data.mmap
                let latest_x = timeseries.latest_timestamp().unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64
                });
                
                let cell_width = match kind {
                    KlineChartKind::Footprint { .. } => 80.0,
                    KlineChartKind::Candles => 4.0,
                };
                let cell_height = 10.0; // Placeholder

                let mut chart = ViewState::new(
                    basis,
                    step,
                    count_decimals(tick_size),
                    ticker_info.clone(),
                    ViewConfig {
                        splits: layout.splits,
                        autoscale: Some(Autoscale::FitToVisible),
                    },
                    cell_width,
                    cell_height,
                );
                chart.state.base_price_y = base_price_y;
                chart.state.latest_x = latest_x;

                let x_translation = match &kind {
                    KlineChartKind::Footprint { .. } => {
                        0.5 * (chart.state.bounds.width / chart.state.scaling)
                            - (chart.state.cell_width / chart.state.scaling)
                    }
                    KlineChartKind::Candles => {
                        0.5 * (chart.state.bounds.width / chart.state.scaling)
                            - (8.0 * chart.state.cell_width / chart.state.scaling)
                    }
                };
                chart.state.translation.x = x_translation;

                let data_source = PlotData::TimeBased(timeseries);
                
                let render_cache_kline = Arc::new(match &data_source {
                    PlotData::TimeBased(ts) => ts.datapoints.values().map(|dp| dp.kline).map(Into::into).collect(),
                    _ => Vec::new(),
                });

                let mut indicators = EnumMap::default();
                for &i in enabled_indicators {
                    let mut indi = indicator::kline::make_empty(i);
                    indi.rebuild_from_source(&data_source);
                    indicators[i] = Some(indi);
                }

                KlineChart {
                    chart,
                    data_source,
                    raw_trades,
                    indicators,
                    fetching_trades: (false, None),
                    request_handler: RequestHandler::new(),
                    kind: kind.clone(),
                    study_configurator: study::Configurator::new(),
                    last_tick: Instant::now(),
                    last_vp_request: Instant::now(),
                    vp_data: None,
                    xaxis_cache: Cache::default(),
                    yaxis_cache: Cache::default(),
                    render_cache_kline,
                    last_visible_range: None,
                }
            }
            Basis::Tick(interval) => {
                let step = PriceStep::from_f32(tick_size);

                let cell_width = match kind {
                    KlineChartKind::Footprint { .. } => 80.0,
                    KlineChartKind::Candles => 4.0,
                };
                let cell_height = match kind {
                    KlineChartKind::Footprint { .. } => 90.0,
                    KlineChartKind::Candles => 8.0,
                };

                let mut chart = ViewState::new(
                    basis,
                    step,
                    count_decimals(tick_size),
                    ticker_info,
                    ViewConfig {
                        splits: layout.splits,
                        autoscale: Some(Autoscale::FitToVisible),
                    },
                    cell_width,
                    cell_height,
                );

                let x_translation = match &kind {
                    KlineChartKind::Footprint { .. } => {
                        0.5 * (chart.state.bounds.width / chart.state.scaling)
                            - (chart.state.cell_width / chart.state.scaling)
                    }
                    KlineChartKind::Candles => {
                        0.5 * (chart.state.bounds.width / chart.state.scaling)
                            - (8.0 * chart.state.cell_width / chart.state.scaling)
                    }
                };
                chart.state.translation.x = x_translation;

                let data_source = PlotData::TickBased(TickAggr::new(interval, step, &raw_trades));
                
                let render_cache_kline = Arc::new(match &data_source {
                    PlotData::TickBased(aggr) => aggr.datapoints.iter().map(|dp| dp.kline).map(Into::into).collect(),
                    _ => Vec::new(),
                });

                let mut indicators = EnumMap::default();
                for &i in enabled_indicators {
                    let mut indi = indicator::kline::make_empty(i);
                    indi.rebuild_from_source(&data_source);
                    indicators[i] = Some(indi);
                }

                KlineChart {
                    chart,
                    data_source,
                    raw_trades,
                    indicators,
                    fetching_trades: (false, None),
                    request_handler: RequestHandler::new(),
                    kind: kind.clone(),
                    study_configurator: study::Configurator::new(),
                    last_tick: Instant::now(),
                    last_vp_request: Instant::now(),
                    vp_data: None,
                    xaxis_cache: Cache::default(),
                    yaxis_cache: Cache::default(),
                    render_cache_kline,
                    last_visible_range: None,
                }
            }
        }
    }

    pub fn update_latest_kline(&mut self, kline: &Kline) {
        match self.data_source {
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_klines(&[*kline]);
                
                // Rebuild cache on new kline
                // Optimization: We could append to existing Vec if Arc is not shared yet, 
                // but since we use BTreeMap as source, full rebuild is safer for order.
                // For high freq, we might want to optimize this further.
                self.rebuild_render_cache();

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_klines(&[*kline]));

                let chart = self.mut_state();

                // Always update latest_x to match the actual K-line data
                // This ensures latest_x reflects the actual data, not system time
                if kline.time != chart.latest_x {
                    let old_latest_x = chart.latest_x;
                    chart.latest_x = kline.time;
                }

                // chart.last_price = Some(PriceInfoLabel::new(kline.close, kline.open));
            }
            PlotData::TickBased(_) => {}
        }
    }

    pub fn kind(&self) -> &KlineChartKind {
        &self.kind
    }

    pub fn ticker_info(&self) -> &TickerInfo {
        &self.chart.state.ticker_info
    }

    fn missing_data_task(&mut self) -> Option<Action> {
        // TODO: Data fetching logic has been moved to the main update loop.
        // This function needs to be re-evaluated or removed.
        // For now, disabling it to fix compilation.
        None
    }

    pub fn reset_request_handler(&mut self) {
        self.request_handler = RequestHandler::new();
        self.fetching_trades = (false, None);
    }

    pub fn raw_trades(&self) -> Vec<Trade> {
        self.raw_trades.clone()
    }

    pub fn set_handle(&mut self, handle: Handle) {
        self.fetching_trades.1 = Some(handle);
    }

    pub fn tick_size(&self) -> f32 {
        self.chart.state.tick_size.to_f32_lossy()
    }

    pub fn study_configurator(&self) -> &study::Configurator<FootprintStudy> {
        &self.study_configurator
    }

    pub fn update_study_configurator(&mut self, message: study::Message<FootprintStudy>) {
        let KlineChartKind::Footprint {
            ref mut studies, ..
        } = self.kind
        else {
            return;
        };

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

    pub fn chart_layout(&self) -> ViewConfig {
        self.chart.state.layout.clone()
    }

    pub fn set_cluster_kind(&mut self, new_kind: ClusterKind) {
        if let KlineChartKind::Footprint {
            ref mut clusters, ..
        } = self.kind
        {
            *clusters = new_kind;
        }

        self.invalidate(None);
    }

    pub fn set_cluster_scaling(&mut self, new_scaling: ClusterScaling) {
        if let KlineChartKind::Footprint {
            ref mut scaling, ..
        } = self.kind
        {
            *scaling = new_scaling;
        }

        self.invalidate(None);
    }

    pub fn basis(&self) -> Basis {
        self.chart.state.basis
    }

    pub fn change_tick_size(&mut self, new_tick_size: f32) {
        let chart = self.mut_state();

        let step = PriceStep::from_f32(new_tick_size);

        chart.cell_height *= new_tick_size / chart.tick_size.to_f32_lossy();
        chart.tick_size = step;

        match self.data_source {
            PlotData::TickBased(ref mut tick_aggr) => {
                tick_aggr.change_tick_size(new_tick_size, &self.raw_trades);
            }
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.change_tick_size(new_tick_size, &self.raw_trades);
            }
        }
        
        self.rebuild_render_cache();

        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.on_ticksize_change(&self.data_source));

        self.invalidate(None);
    }

    pub fn set_basis(&mut self, new_basis: Basis) -> Option<Action> {
        let chart = self.mut_state();
        // chart.last_price = None;
        chart.basis = new_basis;

        match new_basis {
            Basis::Time(interval) => {
                let step = chart.tick_size;
                let timeseries = TimeSeries::<KlineDataPoint>::new(interval, step, &[]);
                self.data_source = PlotData::TimeBased(timeseries);
            }
            Basis::Tick(tick_count) => {
                let step = chart.tick_size;
                let tick_aggr = TickAggr::new(tick_count, step, &self.raw_trades);
                self.data_source = PlotData::TickBased(tick_aggr);
            }
        }
        
        self.rebuild_render_cache();

        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.on_basis_change(&self.data_source));

        self.reset_request_handler();
        self.invalidate(Some(Instant::now()))
    }

    pub fn studies(&self) -> Option<Vec<FootprintStudy>> {
        match &self.kind {
            KlineChartKind::Footprint { studies, .. } => Some(studies.clone()),
            _ => None,
        }
    }

    pub fn set_studies(&mut self, new_studies: Vec<FootprintStudy>) {
        if let KlineChartKind::Footprint {
            ref mut studies, ..
        } = self.kind
        {
            *studies = new_studies;
        }

        self.invalidate(None);
    }

    pub fn insert_trades_buffer(&mut self, trades_buffer: &[Trade]) {
        self.raw_trades.extend_from_slice(trades_buffer);

        match self.data_source {
            PlotData::TickBased(ref mut tick_aggr) => {
                let old_dp_len = tick_aggr.datapoints.len();
                tick_aggr.insert_trades(trades_buffer);

                // if let Some(last_dp) = tick_aggr.datapoints.last() {
                //     self.mut_state().last_price =
                //         Some(PriceInfoLabel::new(last_dp.kline.close, last_dp.kline.open));
                // } else {
                //     self.mut_state().last_price = None;
                // }
                
                self.rebuild_render_cache();

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| {
                        indi.on_insert_trades(trades_buffer, old_dp_len, &self.data_source)
                    });

                self.invalidate(None);
            }
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_trades_existing_buckets(trades_buffer);
                self.rebuild_render_cache();
            }
        }
    }

    pub fn insert_raw_trades(&mut self, raw_trades: Vec<Trade>, is_batches_done: bool) {
        match self.data_source {
            PlotData::TickBased(ref mut tick_aggr) => {
                tick_aggr.insert_trades(&raw_trades);
            }
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_trades_existing_buckets(&raw_trades);
            }
        }
        
        self.rebuild_render_cache();

        self.raw_trades.extend(raw_trades);

        if is_batches_done {
            self.fetching_trades = (false, None);
        }
    }

    pub fn insert_klines(&mut self, req_id: uuid::Uuid, klines_raw: &[Kline]) {
        
        match self.data_source {
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_klines(klines_raw);
                
                timeseries.insert_trades_existing_buckets(&self.raw_trades);
                
                // Update latest_x from timeseries to ensure it matches the data
                // CRITICAL: Always update latest_x to match the actual data, not system time
                if let Some(new_latest_x) = timeseries.latest_timestamp() {
                    let chart = self.mut_state();
                    // Always update to match data, even if it's less than current latest_x
                    // This ensures latest_x reflects the actual K-line data, not system time
                    chart.latest_x = new_latest_x;
                } else {
                    log::warn!("KlineChart: timeseries.latest_timestamp() returned None after inserting {} K-lines", klines_raw.len());
                }
                
                self.rebuild_render_cache();

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.rebuild_from_source(&self.data_source));

                if klines_raw.is_empty() {
                    log::warn!("KlineChart: received empty K-line data for req_id {}", req_id);
                    self.request_handler
                        .mark_failed(req_id, "No data received".to_string());
                } else {
                    self.request_handler.mark_completed(req_id);
                }
                
                // Reset last_visible_range to trigger re-check after data is inserted
                // This ensures that if the visible range is still not covered, we'll fetch more
                self.last_visible_range = None;
                
                self.invalidate(None);
                self.chart.state.vp_needs_update = true;
            }
            PlotData::TickBased(_) => {
                log::warn!("KlineChart: insert_klines called but data_source is TickBased");
            }
        }
    }

    pub fn insert_open_interest(&mut self, req_id: Option<uuid::Uuid>, oi_data: &[OIData]) {
        if let Some(req_id) = req_id {
            if oi_data.is_empty() {
                self.request_handler
                    .mark_failed(req_id, "No data received".to_string());
            } else {
                self.request_handler.mark_completed(req_id);
            }
        }

        if let Some(indi) = self.indicators[KlineIndicator::OpenInterest].as_mut() {
            indi.on_open_interest(oi_data);
        }
    }

    pub fn last_update(&self) -> Instant {
        self.last_tick
    }

    pub fn invalidate(&mut self, now: Option<Instant>) -> Option<Action> {
        // Clone state snapshot before mutable borrow for visible K-line calculation
        let chart_state_snapshot = self.chart.state.clone();
        let chart = &mut self.chart.state;

        if let Some(autoscale) = chart.layout.autoscale {
            match autoscale {
                super::Autoscale::CenterLatest => {
                    let x_translation = match &self.kind {
                        KlineChartKind::Footprint { .. } => {
                            0.5 * (chart.bounds.width / chart.scaling)
                                - (chart.cell_width / chart.scaling)
                        }
                        KlineChartKind::Candles => {
                            0.5 * (chart.bounds.width / chart.scaling)
                                - (8.0 * chart.cell_width / chart.scaling)
                        }
                    };
                    chart.translation.x = x_translation;

                    let calculate_target_y = |kline: exchange::Kline| -> f32 {
                        let y_low = chart.price_to_y(kline.low);
                        let y_high = chart.price_to_y(kline.high);
                        let y_close = chart.price_to_y(kline.close);

                        let mut target_y_translation = -(y_low + y_high) / 2.0;

                        if chart.bounds.height > f32::EPSILON && chart.scaling > f32::EPSILON {
                            let visible_half_height = (chart.bounds.height / chart.scaling) / 2.0;
                            let view_center_y_centered = -target_y_translation;
                            let visible_y_top = view_center_y_centered - visible_half_height;
                            let visible_y_bottom = view_center_y_centered + visible_half_height;
                            let padding = chart.cell_height;

                            if y_close < visible_y_top {
                                target_y_translation = -(y_close - padding + visible_half_height);
                            } else if y_close > visible_y_bottom {
                                target_y_translation = -(y_close + padding - visible_half_height);
                            }
                        }
                        target_y_translation
                    };

                    chart.translation.y = self.data_source.latest_y_midpoint(calculate_target_y);
                    
                    // Clear Y-axis cache when auto scale updates translation.y
                    self.yaxis_cache.clear();
                }
                super::Autoscale::FitToVisible => {
                    // Get visible K-lines using the state snapshot (already cloned above)
                    use crate::chart::visibility;
                    let visible_klines_refs = visibility::get_visible_klines(&chart_state_snapshot, &self.render_cache_kline);
                    
                    // Use unified time range calculation (no padding for rendering)
                    let (start_interval, end_interval) = if let Some(range) = chart_state_snapshot.visible_time_range_ms_for_render() {
                        range
                    } else {
                        // Cannot calculate range, skip autoscale
                        return None;
                    };
                    
                    let (lowest, highest) = if !visible_klines_refs.is_empty() {
                        let lowest = visible_klines_refs.iter()
                            .map(|k| k.low)
                            .fold(f64::INFINITY, |a, b| a.min(b)) as f32;
                        let highest = visible_klines_refs.iter()
                            .map(|k| k.high)
                            .fold(f64::NEG_INFINITY, |a, b| a.max(b)) as f32;
                        
                        (lowest, highest)
                    } else {
                        // Fallback to data source if no K-lines visible on screen
                        if let Some((lowest, highest)) = self
                            .data_source
                            .visible_price_range(start_interval, end_interval)
                        {
                            (lowest, highest)
                        } else {
                            return None;
                        }
                    };
                    
                    // Save debug info to state
                    chart.debug_visible_range = Some((start_interval, end_interval, lowest, highest));
                    
                    // Use cached_y_range stabilization to prevent Y-axis jumping
                    // Round the price range to nice values (same as Y-axis labels)
                    use crate::chart::axes::YAxis;
                    let rounded_range = YAxis::round_price_range(lowest as f64, highest as f64, chart.tick_size);
                    
                    // Check if we should update the Y-axis range based on cached value
                    // This prevents the last K-line from causing Y-axis to jump
                    // CRITICAL: Only update if the change is significant
                    // The threshold in should_update_range is 15% range change or 20% center shift
                    // This ensures small price fluctuations in the last K-line don't cause Y-axis to jump
                    let should_update = YAxis::should_update_range(chart.cached_y_range, rounded_range);
                    
                    // CRITICAL: Only update Y-axis range if change is significant
                    // This prevents the last K-line from causing Y-axis to jump
                    if should_update {
                        // Update cache and use new range
                        chart.cached_y_range = Some(rounded_range);
                        let padded_lowest = rounded_range.1.to_f32_lossy();
                        let padded_highest = rounded_range.0.to_f32_lossy();
                        let price_span = padded_highest - padded_lowest;

                        if price_span > 0.0 && chart.bounds.height > f32::EPSILON {
                            let chart_height = chart.bounds.height;
                            let tick_size = chart.tick_size.to_f32_lossy();

                            if tick_size > 0.0 {
                                // Calculate cell_height so that price_span fits exactly in chart_height
                                // Formula: cell_height = (chart_height * tick_size) / price_span
                                // This ensures that price_span / tick_size * cell_height = chart_height
                                chart.cell_height = (chart_height * tick_size) / price_span;
                                
                                // Set base_price_y to padded_highest so that highest price maps to y=0 in chart coordinates
                                chart.base_price_y = Price::from_f32(padded_highest);
                                
                                // Calculate translation.y to ensure:
                                // - Highest price (y=0 in chart coords) maps to top of screen
                                // - Lowest price (y=price_span_in_chart_coords in chart coords) maps to bottom of screen
                                //
                                // In chart coordinates:
                                // - price_to_y(padded_highest) = 0 (since base_price_y = padded_highest)
                                // - price_to_y(padded_lowest) = (padded_highest - padded_lowest) / tick_size * cell_height = price_span / tick_size * cell_height
                                //
                                // From the calculation: cell_height = (chart_height * tick_size) / price_span
                                // So: price_to_y(padded_lowest) = price_span / tick_size * (chart_height * tick_size / price_span) = chart_height
                                //
                                // In screen coordinates (from shader):
                                // screen_y = (y_chart + translation.y) * scaling + height/2
                                //
                                // We want:
                                // - y_chart=0 (highest) -> screen_y=0 (top)
                                // - y_chart=chart_height (lowest) -> screen_y=height (bottom)
                                //
                                // So: (0 + translation.y) * scaling + height/2 = 0
                                //     => translation.y * scaling = -height/2
                                //     => translation.y = -height/(2*scaling)
                                //
                                // And: (chart_height + translation.y) * scaling + height/2 = height
                                //     => (chart_height + translation.y) * scaling = height/2
                                //     => chart_height * scaling + translation.y * scaling = height/2
                                //     => chart_height * scaling - height/2 = height/2
                                //     => chart_height * scaling = height
                                //     => chart_height = height / scaling
                                //
                                // So we need: chart_height (in chart coords) = height / scaling
                                // But we calculated cell_height using screen pixels (chart.bounds.height),
                                // so chart_height in chart coords = chart.bounds.height / scaling
                                let chart_coord_height = chart_height / chart.scaling;
                                
                                // Calculate translation.y so that y=0 maps to screen top
                                // screen_y = (y_chart + translation.y) * scaling + height/2
                                // For y_chart=0 to map to screen_y=0:
                                // 0 = (0 + translation.y) * scaling + height/2
                                // translation.y = -height/(2*scaling) = -(height/scaling)/2 = -chart_coord_height/2
                                // Calculate translation.y so that y=0 maps to screen top
                                chart.translation.y = -chart_coord_height / 2.0;
                                
                                // Clear Y-axis cache when auto scale updates translation.y and cell_height
                                // This ensures Y-axis labels are recalculated with the new scale
                                self.yaxis_cache.clear();
                            }
                        }
                    } else {
                        // Change is not significant - keep existing Y-axis range unchanged
                        // This prevents the last K-line from causing Y-axis to jump
                        // Do NOT update cell_height, base_price_y, or translation.y
                    }
                }
            }
        }

        // Update debug_visible_range for UI display
        // Use unified time range calculation (no padding for rendering)
        let (start_interval, end_interval) = if let Some(range) = chart.visible_time_range_ms_for_render() {
            range
        } else {
            // Cannot calculate range, skip debug info update
            return None;
        };
        
        // Use unified screen coordinate check to get visible K-lines
        // Clone the necessary state to avoid borrowing conflicts
        let chart_state_snapshot = chart.clone();
        use crate::chart::visibility;
        let visible_klines = visibility::get_visible_klines(&chart_state_snapshot, &self.render_cache_kline);
        
        if !visible_klines.is_empty() {
            let lowest = visible_klines.iter()
                .map(|k| k.low)
                .fold(f64::INFINITY, |a, b| a.min(b)) as f32;
            let highest = visible_klines.iter()
                .map(|k| k.high)
                .fold(f64::NEG_INFINITY, |a, b| a.max(b)) as f32;
            chart.debug_visible_range = Some((start_interval, end_interval, lowest, highest));
        } else {
            // Fallback to data source if no K-lines visible on screen
            if let Some((lowest, highest)) = self
                .data_source
                .visible_price_range(start_interval, end_interval)
            {
                chart.debug_visible_range = Some((start_interval, end_interval, lowest, highest));
            } else {
                chart.debug_visible_range = None;
            }
        }

        // self.chart.cache.clear_all();
        for indi in self.indicators.values_mut().filter_map(Option::as_mut) {
            indi.clear_all_caches();
        }
        
        // Original project doesn't use cached_y_range for Y-axis labels
        // Y-axis labels are recalculated every time based on visible range
        // So we don't need to update cached_y_range here

        if let Some(t) = now {
            self.last_tick = t;
            // Check for visible range changes and data coverage
            self.check_data_update_needed()
        } else {
            None
        }
    }
    
    /// Update Y-axis range cache based on visible K-lines (for stabilization)
    fn update_y_axis_range_cache(&mut self) {
        use crate::chart::axes::YAxis;
        use exchange::util::PriceStep;
        
        // Use screen coordinate check to get visible K-lines (more accurate than time range)
        let visible_klines = self.get_visible_klines_on_screen();
        
        if !visible_klines.is_empty() {
            // Find min low and max high from visible K-lines
            let min_low = visible_klines.iter()
                .map(|k| k.low)
                .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap_or(0.0);
            let max_high = visible_klines.iter()
                .map(|k| k.high)
                .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap_or(0.0);
            
            // Round the price range to nice values
            let chart = &self.chart.state;
            let rounded = YAxis::round_price_range(min_low, max_high, chart.tick_size);
            
            // Apply stabilization: only update cache if change is significant
            // This ensures Y-axis labels remain stable
            if YAxis::should_update_range(chart.cached_y_range, rounded) {
                self.chart.state.cached_y_range = Some(rounded);
            }
            // If change is small, keep using the existing cached_y_range
        }
    }
    
    /// Check if data update is needed based on visible range changes
    fn check_data_update_needed(&mut self) -> Option<Action> {
        let chart = &self.chart.state;
        
        // Use unified time range calculation with 10 intervals padding for data fetching
        let current_range = chart.visible_time_range_ms(10);
        
        let Some((visible_start, visible_end)) = current_range else {
            return None;
        };
        
        // Check if we have data covering the visible range
        let has_data = match &self.data_source {
            PlotData::TimeBased(timeseries) => {
                if timeseries.datapoints.is_empty() {
                    false
                } else {
                    let data_start = timeseries.datapoints.values()
                        .next()
                        .map(|dp| dp.kline.time)
                        .unwrap_or(0);
                    let data_end = timeseries.datapoints.values()
                        .last()
                        .map(|dp| dp.kline.time)
                        .unwrap_or(0);
                    data_start <= visible_start && data_end >= visible_end
                }
            }
            PlotData::TickBased(_) => {
                // Tick-based charts don't need time-based fetching
                return None;
            }
        };
        
        // If we don't have data covering the range, fetch it
        if !has_data {
            self.last_visible_range = Some((visible_start, visible_end));
            
            let ticker_info = chart.ticker_info.clone();
            let timeframe = match chart.basis {
                Basis::Time(tf) => tf,
                Basis::Tick(_) => return None,
            };
            
            let fetch_range = exchange::fetcher::FetchRange::Kline(visible_start, visible_end);
            
            match self.request_handler.add_request(fetch_range) {
                Ok(Some(req_id)) => {
                    let stream = exchange::adapter::StreamKind::Kline {
                        ticker_info,
                        timeframe,
                    };
                    let fetch_spec = exchange::fetcher::FetchSpec {
                        req_id,
                        fetch: fetch_range,
                        stream: Some(stream),
                    };
                    let requests = exchange::fetcher::FetchRequests::from(vec![fetch_spec]);
                    return Some(Action::RequestFetch(requests));
                }
                Ok(None) => {
                    return None;
                }
                Err(e) => {
                    // Request error (overlap, etc.) - silently ignore
                    return None;
                }
            }
        }
        
        // If we have data, check if visible range has changed significantly
        let range_changed = match self.last_visible_range {
            Some((last_start, last_end)) => {
                // Consider range changed if it moved by more than 20% of the range size
                let range_size = visible_end.saturating_sub(visible_start);
                if range_size == 0 {
                    return None;
                }
                let threshold = range_size / 5; // 20% threshold
                let start_diff = visible_start.max(last_start) - visible_start.min(last_start);
                let end_diff = visible_end.max(last_end) - visible_end.min(last_end);
                let changed = start_diff > threshold || end_diff > threshold;
                
                if changed {
                }
                
                changed
            }
            None => {
                self.last_visible_range = Some((visible_start, visible_end));
                false
            }
        };
        
        if range_changed {
            // Range changed significantly, update and fetch
            self.last_visible_range = Some((visible_start, visible_end));
            
            let ticker_info = chart.ticker_info.clone();
            let timeframe = match chart.basis {
                Basis::Time(tf) => tf,
                Basis::Tick(_) => return None,
            };
            
            let fetch_range = exchange::fetcher::FetchRange::Kline(visible_start, visible_end);
            
            match self.request_handler.add_request(fetch_range) {
                Ok(Some(req_id)) => {
                    let stream = exchange::adapter::StreamKind::Kline {
                        ticker_info,
                        timeframe,
                    };
                    let fetch_spec = exchange::fetcher::FetchSpec {
                        req_id,
                        fetch: fetch_range,
                        stream: Some(stream),
                    };
                    let requests = exchange::fetcher::FetchRequests::from(vec![fetch_spec]);
                    Some(Action::RequestFetch(requests))
                }
                Ok(None) => None,
                Err(_) => {
                    None
                }
            }
        } else {
            // Range hasn't changed and we have data, no need to fetch
            None
        }
    }

    pub fn toggle_indicator(&mut self, indicator: KlineIndicator) {
        let prev_indi_count = self.indicators.values().filter(|v| v.is_some()).count();

        if self.indicators[indicator].is_some() {
            self.indicators[indicator] = None;
        } else {
            let mut box_indi = indicator::kline::make_empty(indicator);
            box_indi.rebuild_from_source(&self.data_source);
            self.indicators[indicator] = Some(box_indi);
        }

        if let Some(main_split) = self.chart.state.layout.splits.first() {
            let current_indi_count = self.indicators.values().filter(|v| v.is_some()).count();
            self.chart.state.layout.splits = data::util::calc_panel_splits(
                *main_split,
                current_indi_count,
                Some(prev_indi_count),
            );
        }
    }
    
    /// Check if VP computation is needed and return the request if so
    pub fn check_vp_update_needed(&mut self) -> Option<Action> {
        if !self.chart.state.vp_needs_update {
            return None;
        }
        
        // Debounce VP requests to prevent stuttering (limit to ~3 requests per second)
        if self.last_vp_request.elapsed().as_millis() < 300 {
            return None;
        }

        self.chart.state.vp_needs_update = false; // Reset flag
        self.last_vp_request = Instant::now();

        let symbol = self.chart.state.ticker_info.ticker.to_string();
        
        // CRITICAL: Use exact visible range (NO padding) for VP computation to ensure accuracy
        // Financial calculations must be based on exact visible range, not padded range
        if let Some(time_range) = self.chart.state.visible_time_range_us_for_computation() {
            Some(Action::RequestVpComputation(symbol, time_range))
        } else {
            None
        }
    }
    
    /// Update the stored volume profile data
    /// If bars is empty, clears the VP data instead of storing it
    pub fn set_volume_profile(&mut self, vp: data::compute::vp::VolumeProfile, _time_range: Option<(u64, u64)>) {
        // If VP computation returned empty bars, clear the VP data
        if vp.bars.is_empty() {
            self.chart.state.volume_profile = None;
        } else {
            self.chart.state.volume_profile = Some(vp);
        }
        self.chart.state.vp_needs_update = false;
    }
    
    /// Mark VP as needing update (e.g., when data becomes available)
    pub fn mark_vp_needs_update(&mut self) {
        self.chart.state.vp_needs_update = true;
    }
    
    /// Clear VP data (e.g., when computation fails)
    pub fn clear_volume_profile(&mut self) {
        self.chart.state.volume_profile = None;
    }
    
    /// Get K-lines that are actually visible on screen using screen coordinates
    /// Returns a vector of references to visible K-lines
    fn get_visible_klines_on_screen(&self) -> Vec<&data::kline::KLine> {
        use crate::chart::visibility;
        visibility::get_visible_klines(self.state(), &self.render_cache_kline)
    }
    
    /// Check if there are any K-lines actually visible on screen using screen coordinates
    /// Returns true if at least one K-line overlaps with the visible screen region
    pub fn has_visible_klines_on_screen(&self) -> bool {
        use crate::chart::visibility;
        visibility::has_visible_klines(self.state(), &self.render_cache_kline)
    }

    fn rebuild_render_cache(&mut self) {
        let kline_data: Vec<data::kline::KLine> = match &self.data_source {
            PlotData::TimeBased(timeseries) => {
                timeseries.datapoints.values().map(|dp| dp.kline).map(Into::into).collect()
            },
            PlotData::TickBased(tick_aggr) => {
                tick_aggr.datapoints.iter().map(|dp| dp.kline).map(Into::into).collect()
            },
        };
        self.render_cache_kline = Arc::new(kline_data);
        
        // CRITICAL: Update latest_x from timeseries after rebuilding cache to ensure alignment
        // This ensures latest_x always matches the actual K-line data, not system time
        match &self.data_source {
            PlotData::TimeBased(timeseries) => {
                if let Some(new_latest_x) = timeseries.latest_timestamp() {
                    let chart = self.mut_state();
                    if chart.latest_x != new_latest_x {
                        chart.latest_x = new_latest_x;
                    }
                }
            },
            PlotData::TickBased(_) => {
                // For TickBased, latest_x is managed differently
            },
        }
    }
}
