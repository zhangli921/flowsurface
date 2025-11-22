//! Renderer for Session Volume Profile (S-VP) using instanced drawing.

use iced::wgpu::{self, util::DeviceExt};
use bytemuck::{Pod, Zeroable};
use data::compute::vp::SparseBar;
use crate::chart::ViewState;
use exchange::util::Price;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SvpInstance {
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub color: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<SvpInstance>() == 32);

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SvpUniforms {
    pub projection: [f32; 16],
    pub chart_min_price: f32,
    pub chart_max_price: f32,
    pub chart_min_x: f32,
    pub chart_max_x: f32,
    pub svp_x_offset: f32,
    pub svp_max_width: f32,
    pub price_tick_size: f32,
    pub _padding: f32,
}

pub struct SvpRenderer {
    pub pipeline: wgpu::RenderPipeline,
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    pub bind_group_layout: wgpu::BindGroupLayout,
    
    // Resources updated per frame
    pub instance_buffer: Option<wgpu::Buffer>,
    pub instance_count: u32,
    pub uniform_buffer: Option<wgpu::Buffer>,
    pub bind_group: Option<wgpu::BindGroup>,
}

impl SvpRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("SVP Renderer Shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!("shaders/svp.wgsl"))),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("SVP Bind Group Layout"),
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
            label: Some("SVP Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("SVP Render Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 0,
                                shader_location: 3, // VertexInput position is @location(3)
                            },
                        ],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<SvpInstance>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[
                            // position: vec2<f32> @ 0
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 0,
                                shader_location: 0,
                            },
                            // size: vec2<f32> @ 1
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 8,
                                shader_location: 1,
                            },
                            // color: vec4<f32> @ 2
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 16,
                                shader_location: 2,
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
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let vertex_data: [[f32; 2]; 4] = [
            [0.0, 0.0],
            [1.0, 0.0],
            [1.0, 1.0],
            [0.0, 1.0],
        ];
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("SVP Vertex Buffer"),
            contents: bytemuck::cast_slice(&vertex_data),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_data: [u16; 6] = [0, 1, 2, 0, 2, 3];
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("SVP Index Buffer"),
            contents: bytemuck::cast_slice(&index_data),
            usage: wgpu::BufferUsages::INDEX,
        });

        Self {
            pipeline,
            vertex_buffer,
            index_buffer,
            bind_group_layout,
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
        svp_data: &[SparseBar],
        view_state: &ViewState,
        uniforms: SvpUniforms,
    ) {
        if svp_data.is_empty() {
            self.instance_count = 0;
            return;
        }

        let max_volume = svp_data.iter().map(|b| b.volume).max().unwrap_or(1) as f32;
        
        let visible_region = view_state.state.visible_region(view_state.state.bounds.size());
        let svp_max_width = visible_region.width * 0.25; // Increase width to 25%
        let svp_right_edge = visible_region.x + visible_region.width;
        
        // Calculate height of one price unit in pixels
        let cell_height = view_state.state.cell_height;

        let instances: Vec<SvpInstance> = svp_data.iter().map(|bar| {
            // Convert from compute scaling (x100) to Price scaling (x10^8)
            // Factor = 10^8 / 10^2 = 1_000_000
            let price = Price { units: bar.price_level as i64 * 1_000_000 };
            let y = view_state.state.price_to_y(price);
            
            let width = (bar.volume as f32 / max_volume) * svp_max_width;
            let x = svp_right_edge - width; // Align to right
            
            SvpInstance {
                position: [x, y],
                size: [width, cell_height], 
                color: [0.0, 0.0, 1.0, 0.5], // Blue, semi-transparent
            }
        }).collect();

        self.instance_count = instances.len() as u32;
        
        if self.instance_count > 0 {
             // Instance Buffer
             let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("SVP Instance Buffer"),
                contents: bytemuck::cast_slice(&instances),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
            self.instance_buffer = Some(buffer);
            
            // Uniform Buffer
            let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("SVP Uniform Buffer"),
                contents: bytemuck::cast_slice(&[uniforms]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            
            // Bind Group
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("SVP Bind Group"),
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
            render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            render_pass.set_vertex_buffer(1, instance_buffer.slice(..));
            render_pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..6, 0, 0..self.instance_count);
        }
    }
}
