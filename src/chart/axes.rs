use iced::widget::canvas::{self, Cache, Geometry, Path, Text, Stroke, Frame};
use iced::mouse::Cursor;
use iced::{Color, Point, Rectangle, Size, Theme, Vector};
use crate::chart::{ChartState, Basis};
use exchange::util::Price;
use chrono::{DateTime, TimeZone, Local};

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
                    let (start_ts, end_ts) = state.interval_range(&visible_region);
                    let range_ms = end_ts.saturating_sub(start_ts);
                    
                    if range_ms == 0 { return; }
                    
                    // Determine tick step
                    let step_ms = range_ms / (max_ticks as u64).max(1);
                    
                    let mut current_ts = start_ts;
                    let mut tick_positions = Vec::new();
                    
                    // Simply iterate. Better: align to round times.
                    while current_ts <= end_ts {
                        let x = state.interval_to_x(current_ts);
                        let screen_x = (x + state.translation.x) * state.scaling + content_bounds.width / 2.0;
                        
                        if screen_x >= -50.0 && screen_x <= bounds.width + 50.0 {
                             tick_positions.push((screen_x, current_ts));
                        }
                        
                        current_ts += step_ms;
                    }
                    tick_positions
                }
                Basis::Tick(_) => vec![], 
            };
            
            for (x, ts) in ticks {
                // Draw tick line
                let top = Point::new(x, 0.0);
                let bottom = Point::new(x, 5.0);
                frame.stroke(&Path::line(top, bottom), Stroke::default().with_color(Color::WHITE).with_width(1.0));
                
                // Draw text
                let time_str = if let Some(dt) = DateTime::from_timestamp_millis(ts as i64) {
                     let dt = dt.with_timezone(&Local);
                     dt.format("%H:%M").to_string()
                } else {
                    format!("{}", ts)
                };
                
                let text = Text {
                    content: time_str,
                    position: Point::new(x, 8.0),
                    color: Color::WHITE,
                    size: 10.0.into(),
                    align_x: iced::alignment::Horizontal::Center.into(),
                    align_y: iced::alignment::Vertical::Top.into(),
                    ..Text::default()
                };
                frame.fill_text(text);
            }
        });
        vec![geometry]
    }
}

pub struct YAxis<'a> {
    state: &'a ChartState,
    cache: &'a Cache,
}

impl<'a> YAxis<'a> {
    pub fn new(state: &'a ChartState, cache: &'a Cache) -> Self {
        Self { state, cache }
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
            let (highest_price, lowest_price) = state.price_range(&visible_region);
            
            let range_units = highest_price.units - lowest_price.units;
            if range_units == 0 { return; }
            
            let max_ticks = (bounds.height / 40.0).ceil() as usize; // Ticks every 40px
            let step_units = range_units / (max_ticks as i64).max(1);
            
            let mut current_units = lowest_price.units;
            let end_units = highest_price.units;
            
            // Align to step?
            
            while current_units <= end_units {
                let price = Price { units: current_units };
                let y = state.price_to_y(price);
                
                // Screen Y calculation
                // Screen Y = (y + translation.y) * scaling + height / 2.0
                let screen_y = (y + state.translation.y) * state.scaling + content_bounds.height / 2.0;
                
                if screen_y >= -20.0 && screen_y <= bounds.height + 20.0 {
                    // Draw tick
                    let left = Point::new(0.0, screen_y);
                    let right = Point::new(5.0, screen_y);
                    frame.stroke(&Path::line(left, right), Stroke::default().with_color(Color::WHITE).with_width(1.0));
                    
                    // Draw text
                    let price_str = price.to_string(state.ticker_info.min_ticksize);
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
                
                current_units += step_units;
            }
        });
        vec![geometry]
    }
}
