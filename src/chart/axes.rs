use iced::widget::canvas::{self, Cache, Geometry, Path, Text, Stroke, Frame};
use iced::mouse::Cursor;
use iced::{Color, Point, Rectangle, Size, Theme, Vector};
use crate::chart::{ChartState, Basis};
use exchange::util::{Price, PriceStep};
use chrono::{DateTime, TimeZone, Local, Utc};

const ONE_DAY_MS: u64 = 24 * 60 * 60 * 1000;

pub struct XAxis<'a> {
    state: &'a ChartState,
    cache: &'a Cache,
}

impl<'a> XAxis<'a> {
    pub fn new(state: &'a ChartState, cache: &'a Cache) -> Self {
        Self { state, cache }
    }
}

impl<'a> canvas::Program<()> for XAxis<'a> {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            let state = self.state;
            let content_bounds = state.bounds; // Main chart bounds
            
            // Visible region in chart coordinates
            let visible_region = state.visible_region(content_bounds.size());
            let start_x = visible_region.x;
            let end_x = visible_region.x + visible_region.width;
            
            // Calculate ticks
            // Simple logic: divide width by approx pixel interval (e.g. 100px)
            let max_ticks = (bounds.width / 120.0).ceil() as usize;
            
            let ticks = match state.basis {
                Basis::Time(tf) => {
                    let interval_ms = tf.to_milliseconds();
                    // Use unified time range calculation (no padding for rendering)
                    let (start_ts, end_ts) = if let Some(range) = state.visible_time_range_ms_for_render() {
                        range
                    } else {
                        // Fallback to interval_range if visible_time_range_ms_for_render returns None
                        state.interval_range(&visible_region)
                    };
                    let range_ms = end_ts.saturating_sub(start_ts);
                    
                    if range_ms == 0 { return; }
                    
                    // Use original project's time step calculation logic
                    const ONE_DAY_MS: u64 = 24 * 60 * 60 * 1000;
                    
                    // Calculate optimal time step based on visible range (similar to original project)
                    let target_spacing = 120.0; // pixels between labels
                    let px_per_ms = bounds.width / range_ms as f32;
                    let min_step_ms = (target_spacing / px_per_ms) as u64;
                    
                    // Time step candidates (similar to original project)
                    const TIME_STEPS: &[u64] = &[
                        1000,                    // 1s
                        5 * 1000,                // 5s
                        10 * 1000,               // 10s
                        30 * 1000,               // 30s
                        60 * 1000,               // 1m
                        2 * 60 * 1000,           // 2m
                        5 * 60 * 1000,           // 5m
                        10 * 60 * 1000,          // 10m
                        15 * 60 * 1000,          // 15m
                        30 * 60 * 1000,          // 30m
                        60 * 60 * 1000,          // 1h
                        2 * 60 * 60 * 1000,      // 2h
                        4 * 60 * 60 * 1000,      // 4h
                        6 * 60 * 60 * 1000,      // 6h
                        12 * 60 * 60 * 1000,     // 12h
                        ONE_DAY_MS,              // 1d
                    ];
                    
                    // Find appropriate step
                    let mut step_ms = TIME_STEPS[0];
                    for &candidate in TIME_STEPS {
                        if candidate >= min_step_ms {
                            step_ms = candidate;
                            break;
                        }
                        step_ms = candidate;
                    }
                    
                    // Align first tick to step boundary
                    let first_tick = if start_ts % step_ms == 0 {
                        start_ts
                    } else {
                        ((start_ts / step_ms) + 1) * step_ms
                    };
                    
                    let mut tick_positions = Vec::new();
                    let mut current_ts = first_tick;
                    
                    // Collect all ticks
                    while current_ts <= end_ts {
                        let x = state.interval_to_x(current_ts);
                        let screen_x = (x + state.translation.x) * state.scaling + content_bounds.width / 2.0;
                        
                        if screen_x >= -50.0 && screen_x <= bounds.width + 50.0 {
                            // Check if this is a day boundary (UTC 00:00:00)
                            let is_day_boundary = (current_ts % ONE_DAY_MS) == 0;
                            tick_positions.push((screen_x, current_ts, is_day_boundary));
                        }
                        
                        let prev_ts = current_ts;
                        current_ts = current_ts.saturating_add(step_ms);
                        if current_ts <= prev_ts {
                            break; // Overflow protection
                        }
                    }
                    
                    (tick_positions, step_ms)
                }
                Basis::Tick(_) => (vec![], 60 * 1000), 
            };
            
            let (ticks, step_ms) = ticks;
            
            let mut last_right = f32::NEG_INFINITY;
            for (x, ts, is_day_boundary) in ticks {
                // Draw tick line (longer for day boundaries)
                let top = Point::new(x, 0.0);
                let bottom = Point::new(x, if is_day_boundary { 8.0 } else { 5.0 });
                frame.stroke(&Path::line(top, bottom), Stroke::default().with_color(Color::WHITE).with_width(1.0));
                
                // Draw text - use original project's format_time_label logic
                let (time_str, y_offset) = if let Some(dt) = DateTime::from_timestamp_millis(ts as i64) {
                     let dt_local = dt.with_timezone(&Local);
                     
                     // Determine format based on whether it's a day boundary
                     if is_day_boundary {
                         // For day boundaries, show date (like original project's daily_labels_gen)
                         (dt_local.format("%m-%d").to_string(), 12.0)
                     } else {
                         // For regular ticks, use time format based on step size
                         // This matches original project's format_time_label logic
                         if step_ms < 60 * 1000 {
                             // Less than 1 minute: show seconds
                             (dt_local.format("%H:%M:%S").to_string(), 8.0)
                         } else if step_ms < ONE_DAY_MS {
                             // Less than 1 day: show time
                             (dt_local.format("%H:%M").to_string(), 8.0)
                         } else {
                             // 1 day or more: show date
                             (dt_local.format("%m-%d").to_string(), 10.0)
                         }
                     }
                } else {
                    (format!("{}", ts), 8.0)
                };
                
                // Estimate text width to avoid overlap
                let est_w = (time_str.len() as f32) * 6.0 + 8.0;
                let left = x - est_w * 0.5;
                let right = x + est_w * 0.5;
                
                // Skip if text would overlap with previous text
                if left <= last_right && !is_day_boundary {
                    continue;
                }
                
                let text = Text {
                    content: time_str,
                    position: Point::new(x, y_offset),
                    color: Color::WHITE,
                    size: if is_day_boundary { 11.0 } else { 10.0 }.into(),
                    align_x: iced::alignment::Horizontal::Center.into(),
                    align_y: iced::alignment::Vertical::Top.into(),
                    ..Text::default()
                };
                frame.fill_text(text);
                
                last_right = right;
            }
        });
        vec![geometry]
    }
}

pub struct YAxis<'a> {
    state: &'a ChartState,
    cache: &'a Cache,
    kline_data: Option<&'a [data::kline::KLine]>,
}

impl<'a> YAxis<'a> {
    pub fn new(state: &'a ChartState, cache: &'a Cache, kline_data: Option<&'a [data::kline::KLine]>) -> Self {
        Self { state, cache, kline_data }
    }
    
    /// Choose optimal step size for price range rounding
    /// Returns a PriceStep that is a multiple of tick_size, suitable for the given range
    fn choose_optimal_step(range: f64, tick_size: PriceStep) -> PriceStep {
        let tick_size_f64 = tick_size.to_f32_lossy() as f64;
        
        // Calculate desired number of ticks (aim for 5-10 ticks)
        let desired_ticks = 7.0;
        let ideal_step = range / desired_ticks;
        
        // Find the nearest power-of-10 multiple of tick_size
        let log10 = ideal_step.log10();
        let power = log10.floor() as i32;
        let base = 10f64.powi(power);
        
        // Try multiples: 1x, 2x, 5x, 10x
        let candidates = [1.0, 2.0, 5.0, 10.0];
        let mut best_step = base;
        let mut best_diff = (ideal_step - base).abs();
        
        for &mult in &candidates {
            let candidate = base * mult;
            let diff = (ideal_step - candidate).abs();
            if diff < best_diff {
                best_diff = diff;
                best_step = candidate;
            }
        }
        
        // Round to nearest multiple of tick_size
        let steps = (best_step / tick_size_f64).round() as i64;
        let rounded_step = steps.max(1) as f64 * tick_size_f64;
        
        PriceStep::from_f32(rounded_step as f32)
    }
    
    /// Round price range to nice values with optimal step size
    pub fn round_price_range(min: f64, max: f64, tick_size: PriceStep) -> (Price, Price) {
        let range = max - min;
        if range <= 0.0 {
            return (Price::from_f32(max as f32), Price::from_f32(min as f32));
        }
        
        // Add padding (10% on each side)
        let padding = range * 0.10;
        let padded_min = (min - padding).max(0.0);
        let padded_max = max + padding;
        let padded_range = padded_max - padded_min;
        
        // Choose optimal step
        let step = Self::choose_optimal_step(padded_range, tick_size);
        let step_f64 = step.to_f32_lossy() as f64;
        
        // Round min down and max up to step boundaries
        let rounded_min = (padded_min / step_f64).floor() * step_f64;
        let rounded_max = (padded_max / step_f64).ceil() * step_f64;
        
        (Price::from_f32(rounded_max as f32), Price::from_f32(rounded_min.max(0.0) as f32))
    }
    
    /// Check if new range should update cached range (stabilization)
    /// Returns true if the change is significant enough to warrant an update
    pub fn should_update_range(cached: Option<(Price, Price)>, new: (Price, Price)) -> bool {
        if let Some((cached_high, cached_low)) = cached {
            let cached_range = (cached_high.units - cached_low.units) as f64;
            let new_range = (new.0.units - new.1.units) as f64;
            
            if cached_range <= 0.0 || new_range <= 0.0 {
                return true;
            }
            
            // Calculate relative change
            let range_change = ((new_range - cached_range) / cached_range).abs();
            
            // Check if center shifted significantly
            let cached_center = ((cached_high.units + cached_low.units) as f64) / 2.0;
            let new_center = ((new.0.units + new.1.units) as f64) / 2.0;
            let center_shift = ((new_center - cached_center) / cached_range).abs();
            
            // Update if range changed by more than 15% or center shifted by more than 20%
            // Use larger thresholds for better stability
            range_change > 0.15 || center_shift > 0.20
        } else {
            true // No cache, always update
        }
    }
}

impl<'a> canvas::Program<()> for YAxis<'a> {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            let state = self.state;
            let content_bounds = state.bounds;
            
            let visible_region = state.visible_region(content_bounds.size());
            
            // Calculate price range from visible K-lines if available, otherwise fallback to coordinate-based calculation
            // Use unified time range calculation (no padding for rendering)
            let raw_range = if let Some(klines) = self.kline_data {
                let (start_ts, end_ts) = if let Some(range) = state.visible_time_range_ms_for_render() {
                    range
                } else {
                    // Fallback to interval_range if visible_time_range_ms_for_render returns None
                    state.interval_range(&visible_region)
                };
                // Convert to microseconds for comparison with K-line timestamps
                let start_ts_us = start_ts.checked_mul(1_000).unwrap_or(0);
                let end_ts_us = end_ts.checked_mul(1_000).unwrap_or(0);
                
                // Filter K-lines within visible time range
                let visible_klines: Vec<_> = klines.iter()
                    .filter(|k| k.open_time_us >= start_ts_us && k.open_time_us <= end_ts_us)
                    .collect();
                
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
                    
                    Some((min_low, max_high))
                } else {
                    None
                }
            } else {
                None
            };
            
            // Note: cached_y_range is updated in KlineChart::update_y_axis_range_cache()
            // We don't update it here because state is immutable in draw()
            
            // CRITICAL FIX: Generate labels based on screen Y coordinates, not price units
            // This ensures labels are evenly distributed across the visible screen area
            // regardless of price range or translation/scaling changes
            
            // Calculate the price range corresponding to the entire visible screen height
            // Screen Y coordinates: 0 (top) to bounds.height (bottom)
            let screen_top = 0.0;
            let screen_bottom = bounds.height;
                
            // Convert screen Y to chart Y coordinates
            // screen_y = (chart_y + translation.y) * scaling + height / 2.0
            // chart_y = (screen_y - height / 2.0) / scaling - translation.y
            let chart_y_top = (screen_top - content_bounds.height / 2.0) / state.scaling - state.translation.y;
            let chart_y_bottom = (screen_bottom - content_bounds.height / 2.0) / state.scaling - state.translation.y;
            
            // Convert chart Y coordinates to prices
            let price_at_top = state.y_to_price(chart_y_top);
            let price_at_bottom = state.y_to_price(chart_y_bottom);
            
            // Determine the actual visible price range (may differ from cached range)
            let visible_lowest = price_at_top.min(price_at_bottom);
            let visible_highest = price_at_top.max(price_at_bottom);
            
            let range_units = visible_highest.units - visible_lowest.units;
            if range_units == 0 { return; }
            
            // Generate labels evenly distributed across screen height
            let tick_spacing_px = 40.0; // Target spacing: 40 pixels between ticks
            let num_ticks = (bounds.height / tick_spacing_px).ceil() as usize;
            let num_ticks = num_ticks.max(2); // At least 2 ticks
            
            // Generate labels at evenly spaced screen Y positions
            for i in 0..=num_ticks {
                let screen_y = (i as f32 / num_ticks as f32) * bounds.height;
            
                // Convert screen Y to chart Y, then to price
                let chart_y = (screen_y - content_bounds.height / 2.0) / state.scaling - state.translation.y;
                let price = state.y_to_price(chart_y);
                
                // Round price to nearest tick size for cleaner labels
                let rounded_price = price.round_to_step(state.tick_size);
                
                // Only draw if within reasonable bounds (avoid extreme values)
                if rounded_price.units >= visible_lowest.units.saturating_sub(range_units / 10) &&
                   rounded_price.units <= visible_highest.units.saturating_add(range_units / 10) {
                    // Draw tick
                    let left = Point::new(0.0, screen_y);
                    let right = Point::new(5.0, screen_y);
                    frame.stroke(&Path::line(left, right), Stroke::default().with_color(Color::WHITE).with_width(1.0));
                    
                    // Draw text
                    let price_str = rounded_price.to_string(state.ticker_info.min_ticksize);
                    let text = Text {
                        content: price_str,
                        position: Point::new(8.0, screen_y),
                        color: Color::WHITE,
                        size: 10.0.into(),
                        align_x: iced::alignment::Horizontal::Left.into(),
                        align_y: iced::alignment::Vertical::Center.into(),
                        ..Text::default()
                    };
                    frame.fill_text(text);
                }
            }
        });
        vec![geometry]
    }
}
