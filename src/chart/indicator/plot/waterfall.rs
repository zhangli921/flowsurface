use std::ops::RangeInclusive;

use iced::{Point, Size, Theme, widget::canvas};

use crate::chart::{
    ViewState,
    indicator::plot::{Plot, PlotTooltip, Series, TooltipFn, YScale},
};

pub struct WaterfallPlot<V, T> {
    /// Maps a datapoint to the (delta, cumulative) tuple.
    pub value: V,
    pub bar_width_factor: f32,
    pub padding: f32,
    pub tooltip: Option<TooltipFn<T>>,
    _phantom: std::marker::PhantomData<T>,
}

impl<V, T> WaterfallPlot<V, T> {
    pub fn new(value: V) -> Self {
        Self {
            value,
            bar_width_factor: 0.9,
            padding: 0.05, // Add some default padding
            tooltip: None,
            _phantom: std::marker::PhantomData,
        }
    }

    pub fn with_tooltip<F>(mut self, tooltip: F) -> Self
    where
        F: Fn(&T, Option<&T>) -> PlotTooltip + 'static,
    {
        self.tooltip = Some(Box::new(tooltip));
        self
    }
}

impl<S, V> Plot<S> for WaterfallPlot<V, S::Y>
where
    S: Series,
    S::Y: Copy, // Ensure the data type can be copied
    V: Fn(&S::Y) -> (f32, f32), // Extracts (delta, cumulative)
{
    fn y_extents(&self, datapoints: &S, range: RangeInclusive<u64>) -> Option<(f32, f32)> {
        let mut min_v = f32::MAX;
        let mut max_v = f32::MIN;
        let mut has_data = false;

        let mut update_extents = |val| {
            if val < min_v {
                min_v = val;
            }
            if val > max_v {
                max_v = val;
            }
            has_data = true;
        };

        // The extents are based on the cumulative value.
        // We must also consider the cumulative value of the point just before the
        // visible range, as it's used as the baseline for the first bar.
        if let Some(start_key) = range.start().checked_sub(1) {
            if let Some(y) = datapoints.at(start_key) {
                let (_, cumulative) = (self.value)(y);
                update_extents(cumulative);
            }
        }

        datapoints.for_each_in(range, |_, y| {
            let (_, cumulative) = (self.value)(y);
            update_extents(cumulative);
        });

        if !has_data {
            return None;
        }

        Some((min_v, max_v))
    }

    fn adjust_extents(&self, min: f32, max: f32) -> (f32, f32) {
        if self.padding > 0.0 && max > min {
            let range = max - min;
            let pad = range * self.padding;
            (min - pad, max + pad)
        } else {
            (min, max)
        }
    }

    fn draw(
        &self,
        frame: &mut canvas::Frame,
        ctx: &ViewState,
        theme: &Theme,
        datapoints: &S,
        range: RangeInclusive<u64>,
        scale: &YScale,
    ) {
        let palette = theme.extended_palette();
        let bar_width = ctx.cell_width * self.bar_width_factor;

        let mut prev_cumulative = 0.0;

        // Find the cumulative value of the point just before the visible range
        if let Some(start_key) = range.start().checked_sub(1) {
            if let Some(y) = datapoints.at(start_key) {
                 prev_cumulative = (self.value)(y).1;
            }
        }

        datapoints.for_each_in(range, |x, y| {
            let center_x = ctx.interval_to_x(x);
            let left = center_x - (bar_width / 2.0);

            let (delta, cumulative) = (self.value)(y);

            // Skip drawing the bar if its baseline is 0.0
            if prev_cumulative == 0.0 {
                prev_cumulative = cumulative; // Still update for the next bar
                return; // Skip drawing this bar
            }

            let y1 = scale.to_y(prev_cumulative);
            let y2 = scale.to_y(cumulative);

            let (bar_top, bar_height) = if y2 > y1 {
                (y1, y2 - y1)
            } else {
                (y2, y1 - y2)
            };

            if bar_height > 0.0 {
                let color = if delta >= 0.0 {
                    palette.success.base.color
                } else {
                    palette.danger.base.color
                };

                frame.fill_rectangle(
                    Point::new(left, bar_top),
                    Size::new(bar_width, bar_height),
                    color,
                );
            }

            prev_cumulative = cumulative;
        });
    }

    fn tooltip_fn(&self) -> Option<&TooltipFn<S::Y>> {
        self.tooltip.as_ref()
    }
}
