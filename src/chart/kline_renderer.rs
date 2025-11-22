//! Renderer for K-lines using instanced drawing.

use iced::wgpu::{self, util::DeviceExt};
use bytemuck::{Pod, Zeroable};
use crate::chart::ViewState;
use iced::Rectangle;
use data::kline::KLine;
use exchange::util::Price;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct KlineInstance {
    pub x: f32,
    pub y_open: f32,
    pub y_high: f32,
    pub y_low: f32,
    pub y_close: f32,
    pub width: f32,
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct KlineUniforms {
    pub projection: [f32; 16],
}

pub struct KlineRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    index_buffer: wgpu::Buffer,
    
    // Resources
    instance_buffer: Option<wgpu::Buffer>,
    instance_count: u32,
    uniform_buffer: Option<wgpu::Buffer>,
    bind_group: Option<wgpu::BindGroup>,
}

impl KlineRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Kline Renderer Shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("shaders/kline.wgsl"))),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Kline Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Kline Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Kline Render Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<KlineInstance>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[
                            // x: f32 @ 0
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 0,
                                shader_location: 0,
                            },
                            // y_open: f32 @ 1
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 4,
                                shader_location: 1,
                            },
                            // y_high: f32 @ 2
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 8,
                                shader_location: 2,
                            },
                            // y_low: f32 @ 3
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 12,
                                shader_location: 3,
                            },
                            // y_close: f32 @ 4
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 16,
                                shader_location: 4,
                            },
                            // width: f32 @ 5
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 20,
                                shader_location: 5,
                            },
                            // color: vec4<f32> @ 6
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 24,
                                shader_location: 6,
                            },
                        ],
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        
        let index_data: [u16; 12] = [
            0, 1, 2, 0, 2, 3, // Body
            4, 5, 6, 4, 6, 7, // Wick
        ];
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Kline Index Buffer"),
            contents: bytemuck::cast_slice(&index_data),
            usage: wgpu::BufferUsages::INDEX,
        });

        Self {
            pipeline,
            bind_group_layout,
            index_buffer,
            instance_buffer: None,
            instance_count: 0,
            uniform_buffer: None,
            bind_group: None,
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        klines: &[KLine],
        view_state: &ViewState,
        bounds: &Rectangle,
    ) {
        if klines.is_empty() {
            self.instance_count = 0;
            return;
        }

        let state = &view_state.state;
        let cell_width = state.cell_width;
        
        // Check visible range to avoid creating instances for off-screen candles
        let visible_region = state.visible_region(bounds.size());
        
        let instances: Vec<KlineInstance> = klines
            .iter()
            .map(|k| {
                let time_ms = k.open_time_ns / 1_000_000;
                let x = state.interval_to_x(time_ms);
                let y_open = state.price_to_y(Price::from_f32(k.open as f32));
                let y_high = state.price_to_y(Price::from_f32(k.high as f32));
                let y_low = state.price_to_y(Price::from_f32(k.low as f32));
                let y_close = state.price_to_y(Price::from_f32(k.close as f32));
                
                let is_up = k.close >= k.open;
                let color = if is_up {
                    [0.0, 0.8, 0.0, 1.0] // Green
                } else {
                    [0.8, 0.0, 0.0, 1.0] // Red
                };

                KlineInstance {
                    x,
                    y_open,
                    y_high,
                    y_low,
                    y_close,
                    width: cell_width,
                    color,
                }
            })
            .collect();

        self.instance_count = instances.len() as u32;

        if self.instance_count > 0 {
            // Instance Buffer
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Kline Instance Buffer"),
                contents: bytemuck::cast_slice(&instances),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
            self.instance_buffer = Some(buffer);
            
            // Uniforms
            // Orthographic projection matching ChartRenderer logic
            let left = visible_region.x;
            let right = visible_region.x + visible_region.width;
            let bottom = visible_region.y + visible_region.height;
            let top = visible_region.y;
            
            let projection = [
                [2.0 / (right - left), 0.0, 0.0, 0.0],
                [0.0, 2.0 / (top - bottom), 0.0, 0.0],
                [0.0, 0.0, -1.0, 0.0],
                [-(right + left) / (right - left), -(top + bottom) / (top - bottom), 0.0, 1.0],
            ];
            
            let uniforms = KlineUniforms {
                projection: bytemuck::cast(projection),
            };
            
            // Uniform Buffer
            let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Kline Uniform Buffer"),
                contents: bytemuck::cast_slice(&[uniforms]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            
            // Bind Group
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Kline Bind Group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                ],
            });
            
            self.uniform_buffer = Some(uniform_buffer);
            self.bind_group = Some(bind_group);
        }
    }

    pub fn draw(
        &self,
        render_pass: &mut wgpu::RenderPass<'_>,
    ) {
        if self.instance_count == 0 || self.instance_buffer.is_none() || self.bind_group.is_none() {
            return;
        }

        if let (Some(instance_buffer), Some(bind_group)) = (&self.instance_buffer, &self.bind_group) {
            render_pass.set_pipeline(&self.pipeline);
            render_pass.set_bind_group(0, bind_group, &[]);
            render_pass.set_vertex_buffer(0, instance_buffer.slice(..));
            render_pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..12, 0, 0..self.instance_count);
        }
    }
}
