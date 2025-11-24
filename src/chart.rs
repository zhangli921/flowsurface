pub mod comparison;
pub mod heatmap;
pub mod indicator;
pub mod kline;
mod scale;
pub mod renderer;
pub mod svp_renderer;
pub mod kline_renderer;
pub mod axes;

use crate::style;
use crate::widget::multi_split::{MultiSplit};
use crate::widget::tooltip;
use data::chart::{Autoscale, Basis, PlotData, ViewConfig, indicator::Indicator};
use exchange::TickerInfo;
use exchange::util::{Price, PriceStep};


use iced::{
    padding, Alignment, Element, Length, Point, Rectangle, Size, Theme, Vector,
    widget::{button, center, column, container, mouse_area, row, rule, text, shader, canvas},
};
use chrono::DateTime;
use iced::widget::canvas::Cache;
use crate::chart::renderer::UnifiedChartProgram;
use crate::chart::axes::{XAxis, YAxis};

const ZOOM_SENSITIVITY: f32 = 30.0;

#[derive(Debug, Clone, Copy)]
pub enum AxisScaleClicked {
    X,
    Y,
}

#[derive(Debug, Clone, Copy)]
pub enum Message {
    Translated(Vector),
    Scaled(f32, Vector),
    AutoscaleToggled,
    CrosshairMoved,
    YScaling(f32, f32, bool),
    XScaling(f32, f32, bool),
    BoundsChanged(Rectangle),
    SplitDragged(usize, f32),
    DoubleClick(AxisScaleClicked),
}

pub trait Chart: PlotConstants {
    type IndicatorKind: Indicator;

    fn state(&self) -> &ChartState;
    fn mut_state(&mut self) -> &mut ChartState;
    
    fn chart_data(&self) -> renderer::ChartData;
    fn invalidate_all(&mut self);
    fn invalidate_crosshair(&mut self);
    fn view_indicators(&'_ self, enabled: &[Self::IndicatorKind]) -> Vec<Element<'_, Message>>;
    fn visible_timerange(&self) -> Option<(u64, u64)>;
    fn interval_keys(&self) -> Option<Vec<u64>>;
    fn autoscaled_coords(&self) -> Vector;
    fn supports_fit_autoscaling(&self) -> bool;
    fn is_empty(&self) -> bool;
    
    fn xaxis_cache(&self) -> &Cache;
    fn yaxis_cache(&self) -> &Cache;
    
    /// Get K-line data for calculating price range from visible K-lines
    fn kline_data_for_price_range(&self) -> Option<&[data::kline::KLine]>;
}

pub enum Action {
    ErrorOccurred(data::InternalError),
    RequestFetch(exchange::fetcher::FetchRequests),
    RequestVpComputation(String, data::TimeRange),
}

pub fn update<T: Chart>(chart: &mut T, message: &Message) {
    match message {
        Message::DoubleClick(scale) => {
            let default_chart_width = T::default_cell_width(chart);
            let autoscaled_coords = chart.autoscaled_coords();
            let supports_fit_autoscaling = chart.supports_fit_autoscaling();
            let state = chart.mut_state();

            match scale {
                AxisScaleClicked::X => {
                    state.cell_width = default_chart_width;
                    state.translation = autoscaled_coords;
                }
                AxisScaleClicked::Y => {
                    if supports_fit_autoscaling {
                        state.layout.autoscale = Some(Autoscale::FitToVisible);
                        state.scaling = 1.0;
                    } else {
                        state.layout.autoscale = Some(Autoscale::CenterLatest);
                    }
                }
            }
        }
        Message::Translated(translation) => {
            let state = chart.mut_state();
            if let Some(Autoscale::FitToVisible) = state.layout.autoscale {
                state.translation.x = translation.x;
            } else {
                state.translation = *translation;
                state.layout.autoscale = None;
            }
            state.vp_needs_update = true;
        }
        Message::Scaled(scaling, translation) => {
            let state = chart.mut_state();
            state.scaling = *scaling;
            state.translation = *translation;
            state.layout.autoscale = None;
            state.vp_needs_update = true;
        }
        Message::AutoscaleToggled => {
            let supports_fit_autoscaling = chart.supports_fit_autoscaling();
            let state = chart.mut_state();
            let current_autoscale = state.layout.autoscale;
            state.layout.autoscale = match current_autoscale {
                None => Some(Autoscale::CenterLatest),
                Some(Autoscale::CenterLatest) => {
                    if supports_fit_autoscaling {
                        Some(Autoscale::FitToVisible)
                    } else {
                        None
                    }
                }
                Some(Autoscale::FitToVisible) => None,
            };
            if state.layout.autoscale.is_some() {
                state.scaling = 1.0;
            }
        }
        Message::XScaling(delta, cursor_to_center_x, is_wheel_scroll) => {
            let min_cell_width = T::min_cell_width(chart);
            let max_cell_width = T::max_cell_width(chart);
            let state = chart.mut_state();
            if !(*delta < 0.0 && state.cell_width > min_cell_width || *delta > 0.0 && state.cell_width < max_cell_width) {
                return;
            }
            let is_fit_to_visible_zoom = !is_wheel_scroll && matches!(state.layout.autoscale, Some(Autoscale::FitToVisible));
            let zoom_factor = if is_fit_to_visible_zoom {
                ZOOM_SENSITIVITY / 1.5
            } else if *is_wheel_scroll {
                ZOOM_SENSITIVITY
            } else {
                ZOOM_SENSITIVITY * 3.0
            };
            let new_width = (state.cell_width * (1.0 + delta / zoom_factor)).clamp(min_cell_width, max_cell_width);
            if is_fit_to_visible_zoom {
                let anchor_interval = {
                    let latest_x_coord = state.interval_to_x(state.latest_x);
                    if state.is_interval_x_visible(latest_x_coord) {
                        state.latest_x
                    } else {
                        let visible_region = state.visible_region(state.bounds.size());
                        state.x_to_interval(visible_region.x + visible_region.width)
                    }
                };
                let old_anchor_chart_x = state.interval_to_x(anchor_interval);
                state.cell_width = new_width;
                let new_anchor_chart_x = state.interval_to_x(anchor_interval);
                let shift = new_anchor_chart_x - old_anchor_chart_x;
                state.translation.x -= shift;
            } else {
                let (old_scaling, old_translation_x) = (state.scaling, state.translation.x);
                let latest_x = state.interval_to_x(state.latest_x);
                let is_interval_x_visible = state.is_interval_x_visible(latest_x);
                let cursor_chart_x = if *is_wheel_scroll || !is_interval_x_visible {
                    cursor_to_center_x / old_scaling - old_translation_x
                } else {
                    latest_x / old_scaling - old_translation_x
                };
                let new_cursor_x = match state.basis {
                    Basis::Time(_) => {
                        let cursor_time = state.x_to_interval(cursor_chart_x);
                        state.cell_width = new_width;
                        state.interval_to_x(cursor_time)
                    }
                    Basis::Tick(_) => {
                        let tick_index = cursor_chart_x / state.cell_width;
                        state.cell_width = new_width;
                        tick_index * state.cell_width
                    }
                };
                if *is_wheel_scroll || !is_interval_x_visible {
                    if !new_cursor_x.is_nan() && !cursor_chart_x.is_nan() {
                        state.translation.x -= new_cursor_x - cursor_chart_x;
                    }
                    state.layout.autoscale = None;
                }
            }
            state.vp_needs_update = true;
        }
        Message::YScaling(delta, cursor_to_center_y, is_wheel_scroll) => {
            let min_cell_height = T::min_cell_height(chart);
            let max_cell_height = T::max_cell_height(chart);
            let state = chart.mut_state();
            // Don't disable FitToVisible autoscale - it should continue to work
            // Only disable CenterLatest when user manually zooms
            if state.layout.autoscale == Some(Autoscale::CenterLatest) {
                state.layout.autoscale = None;
            }
            if *delta < 0.0 && state.cell_height > min_cell_height || *delta > 0.0 && state.cell_height < max_cell_height {
                let (old_scaling, old_translation_y) = (state.scaling, state.translation.y);
                let zoom_factor = if *is_wheel_scroll {
                    ZOOM_SENSITIVITY
                } else {
                    ZOOM_SENSITIVITY * 3.0
                };
                let new_height = (state.cell_height * (1.0 + delta / zoom_factor)).clamp(min_cell_height, max_cell_height);
                let cursor_chart_y = cursor_to_center_y / old_scaling - old_translation_y;
                let cursor_price = state.y_to_price(cursor_chart_y);
                state.cell_height = new_height;
                let new_cursor_y = state.price_to_y(cursor_price);
                state.translation.y -= new_cursor_y - cursor_chart_y;
                // Don't disable autoscale on wheel scroll - let FitToVisible continue to work
                // Trigger invalidate to re-apply FitToVisible if enabled
                state.vp_needs_update = true;
            }
        }
        Message::BoundsChanged(bounds) => {
            let state = chart.mut_state();
            let old_center_x = state.bounds.width / 2.0;
            let new_center_x = bounds.width / 2.0;
            let center_delta_x = (new_center_x - old_center_x) / state.scaling;
            state.bounds = *bounds;
            if state.layout.autoscale != Some(Autoscale::CenterLatest) {
                state.translation.x += center_delta_x;
            }
        }
        Message::SplitDragged(split, size) => {
            let state = chart.mut_state();
            if let Some(split) = state.layout.splits.get_mut(*split) {
                *split = (size * 100.0).round() / 100.0;
            }
        }
        Message::CrosshairMoved => return chart.invalidate_crosshair(),
    }
    // Invalidate caches when state changes
    chart.xaxis_cache().clear();
    chart.yaxis_cache().clear();
    
    // Don't clear cached_y_range here - let stabilization work
    // It will be updated in KlineChart::invalidate if needed
    
    chart.invalidate_all();
}

pub fn view<'a, T: Chart>(chart: &'a T, indicators: &'a [T::IndicatorKind], timezone: data::UserTimezone) -> Element<'a, Message> {
    if chart.is_empty() {
        return center(text("Waiting for data...").size(16)).into();
    }
    let state = chart.state();

    // Axis rendering using Canvas
    let axis_labels_x = Element::from(canvas(XAxis::new(state, chart.xaxis_cache()))
        .width(Length::Fill)
        .height(Length::Fill))
        .map(|_| Message::CrosshairMoved);

    let buttons = {
        let (autoscale_btn_placeholder, autoscale_btn_tooltip) = match state.layout.autoscale {
            Some(Autoscale::CenterLatest) => (text("C"), Some("Center last price")),
            Some(Autoscale::FitToVisible) => (text("A"), Some("Auto")),
            None => (text("C"), Some("Toggle autoscaling")),
        };
        let is_active = state.layout.autoscale.is_some();
        let autoscale_button = button(autoscale_btn_placeholder.size(10).align_x(Alignment::Center).align_y(Alignment::Center))
            .height(Length::Fill)
            .on_press(Message::AutoscaleToggled)
            .style(move |theme: &Theme, status| style::button::transparent(theme, status, is_active));
        row![
            iced::widget::space::horizontal(),
            tooltip(autoscale_button, autoscale_btn_tooltip, iced::widget::tooltip::Position::Top),
        ]
        .padding(2)
    };
    let y_labels_width = state.y_labels_width();
    let content = {
        // Update Y-axis range cache before rendering (for stabilization)
        // This needs to be done here because we need mutable access to chart state
        // but view() only has immutable access. We'll update it in YAxis::draw instead
        // by using interior mutability or by calculating it here if possible.
        // For now, we'll let YAxis handle the stabilization logic internally.
        
        let kline_data = chart.kline_data_for_price_range();
        let axis_labels_y = Element::from(canvas(YAxis::new(state, chart.yaxis_cache(), kline_data))
            .width(Length::Fill)
            .height(Length::Fill))
            .map(|_| Message::CrosshairMoved);

        let main_chart_shader = shader::Shader::new(UnifiedChartProgram {
            data: chart.chart_data(),
        });

        let main_chart: Element<_> = row![
            container(main_chart_shader.width(Length::Fill).height(Length::Fill))
                .width(Length::FillPortion(10))
                .height(Length::FillPortion(120)),
            rule::vertical(1).style(style::split_ruler),
            container(mouse_area(axis_labels_y).on_double_click(Message::DoubleClick(AxisScaleClicked::Y)))
                .width(y_labels_width)
                .height(Length::FillPortion(120))
        ]
        .into();

        let indicators = chart.view_indicators(indicators);
        if indicators.is_empty() {
            main_chart
        } else {
            let panels = std::iter::once(main_chart).chain(indicators).collect::<Vec<_>>();
            MultiSplit::new(panels, &state.layout.splits, |index, position| Message::SplitDragged(index, position)).into()
        }
    };
    column![
        content,
        rule::horizontal(1).style(style::split_ruler),
        row![
            container(mouse_area(axis_labels_x).on_double_click(Message::DoubleClick(AxisScaleClicked::X)))
                .padding(padding::right(1))
                .width(Length::FillPortion(10))
                .height(Length::Fixed(26.0)),
            buttons.width(y_labels_width).height(Length::Fixed(26.0))
        ],
        // Debug info display - use Local timezone to match X-axis
        if let Some((start_time, end_time, lowest, highest)) = state.debug_visible_range {
            let start_str = DateTime::from_timestamp_millis(start_time as i64)
                .map(|dt| {
                    let dt_local = dt.with_timezone(&chrono::Local);
                    dt_local.format("%Y-%m-%d %H:%M:%S").to_string()
                })
                .unwrap_or_else(|| format!("{} ms", start_time));
            let end_str = DateTime::from_timestamp_millis(end_time as i64)
                .map(|dt| {
                    let dt_local = dt.with_timezone(&chrono::Local);
                    dt_local.format("%Y-%m-%d %H:%M:%S").to_string()
                })
                .unwrap_or_else(|| format!("{} ms", end_time));
            
            container(
                text(format!(
                    "时间范围: {} - {} | 价格范围: {:.8} - {:.8}",
                    start_str, end_str, lowest, highest
                ))
                .size(10)
                .style(move |_theme: &Theme| {
                    iced::widget::text::Style {
                        color: Some(iced::Color::from_rgb(0.7, 0.7, 0.7)),
                    }
                })
            )
            .padding(padding::left(4).top(2))
            .width(Length::Fill)
            .height(Length::Shrink)
        } else {
            container(text("").size(10))
                .width(Length::Fill)
                .height(Length::Shrink)
        }
    ]
    .padding(padding::left(1).right(1).bottom(1))
    .into()
}

pub trait PlotConstants {
    fn min_scaling(&self) -> f32;
    fn max_scaling(&self) -> f32;
    fn max_cell_width(&self) -> f32;
    fn min_cell_width(&self) -> f32;
    fn max_cell_height(&self) -> f32;
    fn min_cell_height(&self) -> f32;
    fn default_cell_width(&self) -> f32;
}

#[derive(Clone, Debug)]
pub struct ChartState {
    pub bounds: Rectangle,
    pub translation: Vector,
    pub scaling: f32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub basis: Basis,
    // pub last_price: Option<PriceInfoLabel>,
    pub base_price_y: Price,
    pub latest_x: u64,
    pub tick_size: PriceStep,
    pub decimals: usize,
    pub ticker_info: TickerInfo,
    pub layout: ViewConfig,
    pub volume_profile: Option<data::compute::vp::VolumeProfile>,
    pub vp_needs_update: bool,
    // Cached Y-axis range for stabilization (rounded min/max prices)
    pub cached_y_range: Option<(Price, Price)>,
    // Debug info for visible range
    pub debug_visible_range: Option<(u64, u64, f32, f32)>, // (start_time, end_time, lowest_price, highest_price)
}

impl ChartState {
    #[inline]
    fn price_unit() -> i64 { 10i64.pow(Price::PRICE_SCALE as u32) }
    pub fn visible_region(&self, size: Size) -> Rectangle {
        let width = size.width / self.scaling;
        let height = size.height / self.scaling;
        Rectangle {
            x: -self.translation.x - width / 2.0,
            y: -self.translation.y - height / 2.0,
            width,
            height,
        }
    }
    pub fn is_interval_x_visible(&self, interval_x: f32) -> bool {
        let region = self.visible_region(self.bounds.size());
        interval_x >= region.x && interval_x <= region.x + region.width
    }
    pub fn interval_range(&self, region: &Rectangle) -> (u64, u64) {
        match self.basis {
            Basis::Tick(_) => (self.x_to_interval(region.x + region.width), self.x_to_interval(region.x)),
            Basis::Time(_) => {
                // No padding - use exact visible region boundaries
                (
                    self.x_to_interval(region.x),
                    self.x_to_interval(region.x + region.width),
                )
            }
        }
    }

    pub fn visible_time_range_us(&self) -> Option<data::TimeRange> {
        match self.basis {
            Basis::Time(_) => {
                let region = self.visible_region(self.bounds.size());
                let (start, end) = self.interval_range(&region);
                Some(data::TimeRange {
                    start_us: start.checked_mul(1_000).unwrap_or(0), // Convert ms to microseconds
                    end_us: end.checked_mul(1_000).unwrap_or(0),
                })
            }
            _ => None,
        }
    }
    pub fn price_range(&self, region: &Rectangle) -> (Price, Price) {
        let highest = self.y_to_price(region.y);
        let lowest = self.y_to_price(region.y + region.height);
        (highest, lowest)
    }
    pub fn interval_to_x(&self, value: u64) -> f32 {
        match self.basis {
            Basis::Time(timeframe) => {
                let interval = timeframe.to_milliseconds() as f64;
                let cell_width = f64::from(self.cell_width);
                let diff = value as f64 - self.latest_x as f64;
                (diff / interval * cell_width) as f32
            }
            Basis::Tick(_) => -((value as f32) * self.cell_width),
        }
    }
    pub fn x_to_interval(&self, x: f32) -> u64 {
        match self.basis {
            Basis::Time(timeframe) => {
                let interval = timeframe.to_milliseconds();
                if x <= 0.0 {
                    let diff = (-x / self.cell_width * interval as f32) as u64;
                    self.latest_x.saturating_sub(diff)
                } else {
                    let diff = (x / self.cell_width * interval as f32) as u64;
                    self.latest_x.saturating_add(diff)
                }
            }
            Basis::Tick(_) => {
                let tick = -(x / self.cell_width);
                tick.round() as u64
            }
        }
    }
    pub fn price_to_y(&self, price: Price) -> f32 {
        if self.tick_size.units == 0 {
            let one = Self::price_unit() as f32;
            let delta_units = (self.base_price_y.units - price.units) as f32;
            return (delta_units / one) * self.cell_height;
        }
        let delta_units = self.base_price_y.units - price.units;
        let ticks = (delta_units as f32) / (self.tick_size.units as f32);
        ticks * self.cell_height
    }
    pub fn y_to_price(&self, y: f32) -> Price {
        if self.tick_size.units == 0 {
            let one = Self::price_unit() as f32;
            let delta_units = ((y / self.cell_height) * one).round() as i64;
            return Price::from_units(self.base_price_y.units - delta_units);
        }
        let ticks: f32 = y / self.cell_height;
        let delta_units = (ticks * self.tick_size.units as f32).round() as i64;
        Price::from_units(self.base_price_y.units - delta_units)
    }
    pub fn y_labels_width(&self) -> Length {
        let precision = self.ticker_info.min_ticksize;
        let value = self.base_price_y.to_string(precision);
        let width = (value.len() as f32 * 8.0).max(72.0); // 8.0 is an approximation for char width
        Length::Fixed(width.ceil())
    }
}

#[derive(Debug, Clone)]
pub struct ViewState {
    pub state: ChartState,
    // Caches are removed because they are based on canvas, which is not Sync.
    // pub cache: Caches,
}

impl ViewState {
    pub fn new(
        basis: Basis,
        tick_size: PriceStep,
        decimals: usize,
        ticker_info: TickerInfo,
        layout: ViewConfig,
        cell_width: f32,
        cell_height: f32,
    ) -> Self {
        Self {
            state: ChartState {
                bounds: Rectangle::default(),
                translation: Vector::default(),
                scaling: 1.0,
                cell_width,
                cell_height,
                basis,
                base_price_y: Price::from_f32_lossy(0.0),
                latest_x: 0,
                tick_size,
                decimals,
                ticker_info,
                layout,
                volume_profile: None,
                vp_needs_update: true,
                cached_y_range: None,
                debug_visible_range: None,
            },
            // cache: Caches::default(),
        }
    }
}
