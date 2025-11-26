//! Visibility checking utilities for chart elements.
//! 
//! This module provides functions to check if chart elements (e.g., K-lines) are visible
//! on screen based on screen coordinates. This is more accurate than time-range-based
//! filtering because it accounts for zoom, pan, and other view transformations.

use crate::chart::ChartState;
use data::kline::KLine;

/// Get K-lines that are actually visible on screen using screen coordinates.
/// 
/// This function calculates the screen coordinates of each K-line and checks if it
/// overlaps with the visible screen region [0, bounds.width].
/// 
/// # Arguments
/// 
/// * `chart_state` - The chart state containing view transformation parameters
/// * `klines` - Slice of K-lines to check for visibility
/// 
/// # Returns
/// 
/// A vector of references to visible K-lines.
pub fn get_visible_klines<'a>(
    chart_state: &ChartState,
    klines: &'a [KLine],
) -> Vec<&'a KLine> {
    if chart_state.bounds.width <= 0.0 
        || chart_state.bounds.height <= 0.0 
        || chart_state.scaling <= f32::EPSILON {
        return Vec::new();
    }
    if klines.is_empty() {
        return Vec::new();
    }
    
    // Calculate screen coordinates for K-lines (unified logic)
    let base_time_ms = (klines[0].open_time_us / 1_000) as f64;
    let interval_ms = match chart_state.basis {
        data::chart::Basis::Time(tf) => tf.to_milliseconds() as f64,
        _ => 1.0,
    };
    let cell_width = chart_state.cell_width as f64;
    let latest_x = chart_state.latest_x as f64;
    let scale_factor = cell_width / interval_ms.max(1.0);
    let transform_x = (scale_factor as f32) * chart_state.scaling;
    let base_diff = base_time_ms - latest_x;
    let transform_y = ((base_diff * scale_factor) as f32 * chart_state.scaling) 
        + (chart_state.translation.x * chart_state.scaling);
    
    let candle_width = chart_state.cell_width;
    let half_candle_width_screen = (candle_width / 2.0) * chart_state.scaling;
    
    // Filter K-lines that are visible on screen
    klines.iter()
        .filter(|k| {
            let kline_time_ms = (k.open_time_us / 1_000) as f64;
            let time_offset = (kline_time_ms - base_time_ms) as f32;
            let kline_center_x_screen = (time_offset * transform_x) + transform_y;
            let kline_left_screen = kline_center_x_screen - half_candle_width_screen;
            let kline_right_screen = kline_center_x_screen + half_candle_width_screen;
            // K-line is visible if it overlaps with screen bounds [0, bounds.width]
            kline_left_screen <= chart_state.bounds.width && kline_right_screen >= 0.0
        })
        .collect()
}

/// Check if there are any K-lines actually visible on screen using screen coordinates.
/// 
/// This is a convenience function that returns `true` if at least one K-line is visible.
/// 
/// # Arguments
/// 
/// * `chart_state` - The chart state containing view transformation parameters
/// * `klines` - Slice of K-lines to check for visibility
/// 
/// # Returns
/// 
/// `true` if at least one K-line is visible on screen, `false` otherwise.
pub fn has_visible_klines(
    chart_state: &ChartState,
    klines: &[KLine],
) -> bool {
    !get_visible_klines(chart_state, klines).is_empty()
}

