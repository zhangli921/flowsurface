use iced::advanced::layout::{Layout};
use iced::advanced::renderer;
use iced::mouse::{self, Cursor};
use std::sync::Arc;
use iced::{event, Rectangle, Vector, Point};
use iced::widget::shader;
use iced::wgpu::{self, util::DeviceExt};
use bytemuck;


use crate::chart::{ViewState, Message};
use data::compute::vp::SparseBar;

#[derive(Default, Debug, Clone, Copy)]
pub enum Interaction {
    #[default]
    None,
    Zoomin {
        last_position: Point,
    },
    Panning {
        translation: Vector,
        start: Point,
    },
    Ruler {
        start: Option<Point>,
    },
}

/// A program that can be used with `iced::widget::shader::Shader` to render the chart.
#[derive(Debug)]
pub struct UnifiedChartProgram {
    pub data: ChartData,
}

#[derive(Debug, Clone)]
pub struct ChartData {
    pub kline_data: Arc<Vec<data::kline::KLine>>,
    pub svp_data: Arc<Vec<SparseBar>>,
    pub view_state: ViewState,
}

impl<M> shader::Program<M> for UnifiedChartProgram
where
    M: From<crate::chart::Message>,
{
    type State = Interaction;
    type Primitive = ChartRenderer;

    fn update(
        &self,
        state: &mut Self::State,
        event: &event::Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<iced::widget::Action<M>> {
        // Check if bounds changed
        if bounds != self.data.view_state.state.bounds {
             return Some(iced::widget::Action::publish(
                 crate::chart::Message::BoundsChanged(bounds).into(),
             ));
        }

        match event {
            event::Event::Mouse(mouse_event) => match mouse_event {
                mouse::Event::ButtonPressed(mouse::Button::Left) => {
                    if let Some(position) = cursor.position() {
                        if bounds.contains(position) {
                            *state = Interaction::Panning {
                                translation: self.data.view_state.state.translation,
                                start: position,
                            };
                        }
                    }
                }
                mouse::Event::ButtonReleased(mouse::Button::Left) => {
                    if matches!(*state, Interaction::Panning { .. }) {
                        *state = Interaction::None;
                    }
                }
                mouse::Event::CursorMoved { position } => {
                    if let Interaction::Panning { translation, start } = *state {
                        let delta = *position - start;
                        // Lock Y axis panning - only allow X panning via chart drag
                        let new_translation = translation + Vector::new(delta.x, 0.0);
                        return Some(iced::widget::Action::publish(
                            crate::chart::Message::Translated(new_translation).into(),
                        ));
                    }
                    // Emit CrosshairMoved
                    // return Some(iced::widget::Action::publish(
                    //     crate::chart::Message::CrosshairMoved.into(),
                    // ));
                }
                mouse::Event::WheelScrolled { delta } => {
                    if let Some(cursor_position) = cursor.position_in(bounds) {
                        let delta_y = match delta {
                            mouse::ScrollDelta::Lines { y, .. } => *y,
                            mouse::ScrollDelta::Pixels { y, .. } => *y,
                        };
                        
                        if delta_y != 0.0 {
                             return Some(iced::widget::Action::publish(
                                crate::chart::Message::XScaling(delta_y, cursor_position.x, true).into(),
                            ));
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
        None
    }

    fn draw(
        &self,
        _state: &Self::State,
        _cursor: mouse::Cursor,
        bounds: Rectangle,
    ) -> Self::Primitive {
        ChartRenderer {
            bounds,
            kline_data: self.data.kline_data.clone(),
            svp_data: self.data.svp_data.clone(),
            view_state: self.data.view_state.clone(),
        }
    }
}

/// The custom primitive that will be rendered by the WGPU pipeline.
#[derive(Debug, Clone)]
pub struct ChartRenderer {
    pub bounds: Rectangle,
    pub kline_data: Arc<Vec<data::kline::KLine>>,
    pub svp_data: Arc<Vec<SparseBar>>,
    pub view_state: ViewState,
}

use crate::chart::svp_renderer::SvpRenderer;
use crate::chart::kline_renderer::KlineRenderer;

// The WGPU renderer state that holds all rendering resources
pub struct ChartWgpuRenderer {
    kline_renderer: KlineRenderer,
    svp_renderer: Option<SvpRenderer>,
}

impl shader::Primitive for ChartRenderer {
    type Renderer = ChartWgpuRenderer;

    fn initialize(
        &self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Self::Renderer {
        log::info!("Initializing ChartWgpuRenderer");
        
        // Initialize renderers
        let kline_renderer = KlineRenderer::new(device, format);
        let svp_renderer = SvpRenderer::new(device, format);
        
        ChartWgpuRenderer {
            kline_renderer,
            svp_renderer: Some(svp_renderer),
        }
    }

    fn prepare(
        &self,
        renderer: &mut Self::Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &shader::Viewport,
    ) {
        // Prepare K-line data
        renderer.kline_renderer.prepare(device, queue, &self.kline_data, &self.view_state, bounds);

        // Prepare SVP data for rendering
        if !self.svp_data.is_empty() {
            if let Some(svp_renderer) = &mut renderer.svp_renderer {
                // Delegate preparation to SvpRenderer
                svp_renderer.prepare(device, queue, &self.svp_data, &self.view_state, ());
            }
        }
    }
    
    fn draw(
        &self,
        renderer: &Self::Renderer,
        render_pass: &mut wgpu::RenderPass<'_>,
    ) -> bool {
        // Draw SVP (background layer)
        if let Some(svp_renderer) = &renderer.svp_renderer {
            svp_renderer.draw(render_pass);
        }

        // Draw K-lines (foreground layer)
        renderer.kline_renderer.draw(render_pass);
        
        // Return true to indicate we handled the rendering
        true
    }
}
