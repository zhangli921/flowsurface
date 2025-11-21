use iced::advanced::layout::{Layout};
use iced::advanced::renderer;
use iced::mouse::{self, Cursor};
use iced::{event, Rectangle, Vector, Point};
use iced::widget::shader;
use iced::wgpu;


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
    pub kline_data: Vec<data::kline::KLine>,
    pub svp_data: Vec<SparseBar>,
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
        _state: &mut Self::State,
        _event: &event::Event,
        _bounds: Rectangle,
        _cursor: Cursor,
    ) -> Option<iced::widget::Action<M>> {
        // TODO: Implement event handling logic here, adapting from the old `canvas_interaction` function.
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
    pub kline_data: Vec<data::kline::KLine>,
    pub svp_data: Vec<SparseBar>,
    pub view_state: ViewState,
}

// This is the placeholder for our custom WGPU renderer state
pub struct ChartWgpuRenderer;

impl shader::Primitive for ChartRenderer {
    type Renderer = ChartWgpuRenderer;

    fn initialize(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _format: wgpu::TextureFormat,
    ) -> Self::Renderer {
        ChartWgpuRenderer
    }

    fn prepare(
        &self,
        _renderer: &mut Self::Renderer,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _bounds: &Rectangle,
        _viewport: &shader::Viewport,
    ) {
        // Prepare data for rendering
    }
    
    fn draw(
        &self,
        _renderer: &Self::Renderer,
        _render_pass: &mut wgpu::RenderPass<'_>,
    ) -> bool {
        // Draw the primitive
        false
    }
}
