//! Renderer for Session Volume Profile (S-VP) using instanced drawing with GPU-side geometry generation.

use iced::wgpu::{self, util::DeviceExt};
use bytemuck::{Pod, Zeroable};
use data::compute::vp::SparseBar;
use crate::chart::ViewState;
use std::sync::Arc;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SvpInstance {
    pub price: f32,
    pub volume: f32,
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct SvpUniforms {
    // x: unused, y: unused, z: price_scale, w: price_offset
    pub transform: [f32; 4],
    pub screen_size: [f32; 2],
    // x: svp_x_start, y: volume_scale
    pub svp_params: [f32; 2],
    pub bar_height: f32,
    pub _padding: [f32; 7],
}

pub struct SvpRenderer {
    pub pipeline: wgpu::RenderPipeline,
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    pub bind_group_layout: wgpu::BindGroupLayout,
    
    // Persistent Resources
    pub instance_buffer: Option<wgpu::Buffer>,
    pub instance_capacity: usize,
    pub instance_count: u32,
    
    pub uniform_buffer: Option<wgpu::Buffer>,
    pub bind_group: Option<wgpu::BindGroup>,
    
    // Data Versioning
    last_data_id: Option<usize>,
    last_data_len: usize,
    cached_max_volume: f32,
    base_price_units: i64,
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
                                shader_location: 3, // VertexInput position
                            },
                        ],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<SvpInstance>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[
                            // price: f32 @ 0
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 0,
                                shader_location: 0,
                            },
                            // volume: f32 @ 4
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 4,
                                shader_location: 1,
                            },
                            // color: vec4<f32> @ 8
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 8,
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
            instance_capacity: 0,
            instance_count: 0,
            uniform_buffer: None,
            bind_group: None,
            last_data_id: None,
            last_data_len: 0,
            cached_max_volume: 1.0,
            base_price_units: 0,
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        svp_data: &Arc<Vec<SparseBar>>,
        view_state: &ViewState,
        _old_uniforms: (), // Ignored, we calculate new ones
    ) {
        if svp_data.is_empty() {
            self.instance_count = 0;
            return;
        }

        let current_id = svp_data.as_ptr() as usize;
        let data_changed = self.last_data_id != Some(current_id) || self.last_data_len != svp_data.len();

        if data_changed {
            let state = &view_state.state; // Access view state for hack

            self.cached_max_volume = svp_data.iter().map(|b| b.volume).max().unwrap_or(1) as f32;
            
            // Find base price (min price level in profile)
            let min_price_level = svp_data.iter().map(|b| b.price_level).min().unwrap_or(0);
            
            // Convert base price level (scaled *100) to Price units (*10^8).
            // Factor: 1,000,000.
            self.base_price_units = (min_price_level as i64) * 1_000_000;
            
            let instances: Vec<SvpInstance> = svp_data.iter().map(|bar| {
                let bar_price_units = (bar.price_level as i64) * 1_000_000;
                let price_offset_units = bar_price_units - self.base_price_units;
                
                // Convert to f32 offset
                let price_offset = price_offset_units as f32;
                
                SvpInstance {
                    price: price_offset, 
                    volume: bar.volume as f32,
                    color: [0.0, 0.0, 1.0, 0.5], // Blue, semi-transparent
                }
            }).collect();

            self.instance_count = instances.len() as u32;

            if self.instance_buffer.is_none() || self.instance_capacity < instances.len() {
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("SVP Instance Buffer"),
                    contents: bytemuck::cast_slice(&instances),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                });
                self.instance_buffer = Some(buffer);
                self.instance_capacity = instances.len();
            } else {
                if let Some(buffer) = &self.instance_buffer {
                    queue.write_buffer(buffer, 0, bytemuck::cast_slice(&instances));
                }
            }
            
            self.last_data_id = Some(current_id);
            self.last_data_len = svp_data.len();
        }

        if self.instance_count > 0 {
            let state = &view_state.state;
            
            // Calculate Uniforms
            // Y Transform (Price -> Screen Y)
            // Using the same logic as KlineRenderer but adapted for SvpInstance format.
            // SvpInstance.price = (price_units - base_price_units) as f32
            // y_screen = price_offset * Z + W
            
            let tick_units_f = state.tick_size.units as f32;
            let units_per_pixel_factor = state.cell_height / tick_units_f.max(1.0);
            
            // Z: Scale factor for instance price offset
            // y_chart = -offset * (1/tick * cell) ... (inherited from Kline derivation)
            let transform_z = -units_per_pixel_factor * state.scaling;
            
            // W: Offset constant
            // Needs to map base_price_units to screen Y
            let base_diff_units = (state.base_price_y.units - self.base_price_units) as f32;
            let y_chart_offset = base_diff_units * units_per_pixel_factor;
            
            let transform_w = (y_chart_offset + state.translation.y) * state.scaling + state.bounds.height / 2.0;

            // X Parameters
            // Use screen width for max width calculation to keep it proportional to screen
            let svp_max_width = state.bounds.width * 0.25;
            
            // Right alignment logic: Pin to right edge of SCREEN
            let svp_x_start = state.bounds.width;
            let volume_scale = -(svp_max_width / self.cached_max_volume.max(1.0));

            let uniforms = SvpUniforms {
                transform: [0.0, 0.0, transform_z, transform_w],
                screen_size: [state.bounds.width, state.bounds.height],
                svp_params: [svp_x_start, volume_scale],
                bar_height: state.cell_height * state.scaling,
                _padding: [0.0; 7],
            };

            if self.uniform_buffer.is_none() {
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("SVP Uniform Buffer"),
                    contents: bytemuck::cast_slice(&[uniforms]),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });
                self.uniform_buffer = Some(buffer);
                
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("SVP Bind Group"),
                    layout: &self.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.uniform_buffer.as_ref().unwrap().as_entire_binding(),
                        },
                    ],
                });
                self.bind_group = Some(bind_group);
            } else {
                queue.write_buffer(self.uniform_buffer.as_ref().unwrap(), 0, bytemuck::cast_slice(&[uniforms]));
            }
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
