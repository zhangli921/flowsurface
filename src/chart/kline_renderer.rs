//! Renderer for K-lines using instanced drawing with GPU-side geometry generation.

use iced::wgpu::{self, util::DeviceExt};
use bytemuck::{Pod, Zeroable};
use crate::chart::ViewState;
use iced::Rectangle;
use data::kline::KLine;
use std::sync::Arc;
use exchange::util::Price;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct KlineInstance {
    pub time_offset: f32,
    pub open: f32,
    pub high: f32,
    pub low: f32,
    pub close: f32,
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct KlineUniforms {
    // x: time_scale, y: time_offset, z: price_scale, w: price_offset
    pub transform: [f32; 4],
    pub screen_size: [f32; 2],
    pub candle_width: f32,
    pub _padding: f32,
}

pub struct KlineRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    index_buffer: wgpu::Buffer,
    
    // Persistent Resources
    instance_buffer: Option<wgpu::Buffer>,
    instance_capacity: usize,
    instance_count: u32,
    
    uniform_buffer: Option<wgpu::Buffer>,
    bind_group: Option<wgpu::BindGroup>,
    
    // Data Versioning
    last_data_id: Option<usize>,
    last_data_len: usize,
    base_time_ms: f64,
    base_price_units: i64,
    
    // Cache last uniform values to avoid unnecessary buffer writes
    last_uniforms: Option<KlineUniforms>,
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
                            // time_offset: f32 @ 0
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 0,
                                shader_location: 0,
                            },
                            // open: f32 @ 4
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 4,
                                shader_location: 1,
                            },
                            // high: f32 @ 8
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 8,
                                shader_location: 2,
                            },
                            // low: f32 @ 12
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 12,
                                shader_location: 3,
                            },
                            // close: f32 @ 16
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32,
                                offset: 16,
                                shader_location: 4,
                            },
                            // color: vec4<f32> @ 20
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 20,
                                shader_location: 5,
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
            instance_capacity: 0,
            instance_count: 0,
            uniform_buffer: None,
            bind_group: None,
            last_data_id: None,
            last_data_len: 0,
            base_time_ms: 0.0,
            base_price_units: 0,
            last_uniforms: None,
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        klines: &Arc<Vec<KLine>>,
        view_state: &ViewState,
        bounds: &Rectangle,
    ) {
        if klines.is_empty() {
            self.instance_count = 0;
            return;
        }

        let current_id = klines.as_ptr() as usize;
        let data_changed = self.last_data_id != Some(current_id) || self.last_data_len != klines.len();

        if data_changed {
            // 1. Update Instance Data (CPU Heavy, but only on data change)
            
            // Use unified coordinate system from ChartState
            // This ensures K-lines and Volume Profile use the same base for alignment
            let state = &view_state.state;
            let data_base_time_ms = state.data_base_time_ms();
            let data_base_price_units = state.data_base_price_units();
            
            // Store unified base for data offset calculation
            self.base_time_ms = data_base_time_ms as f64;
            self.base_price_units = data_base_price_units;
            
            let instances: Vec<KlineInstance> = klines
                .iter()
                .map(|k| {
                    let time_ms = (k.open_time_us / 1_000) as f64;
                    let time_offset = (time_ms - self.base_time_ms) as f32;
                    
                    // Price offsets relative to base_price_units
                    let open_offset = (Price::from_f32(k.open as f32).units - self.base_price_units) as f32;
                    let high_offset = (Price::from_f32(k.high as f32).units - self.base_price_units) as f32;
                    let low_offset = (Price::from_f32(k.low as f32).units - self.base_price_units) as f32;
                    let close_offset = (Price::from_f32(k.close as f32).units - self.base_price_units) as f32;
                    
                    let is_up = k.close >= k.open;
                    let color = if is_up {
                        [0.0, 0.8, 0.0, 1.0] // Green
                    } else {
                        [0.8, 0.0, 0.0, 1.0] // Red
                    };

                    KlineInstance {
                        time_offset,
                        open: open_offset,
                        high: high_offset,
                        low: low_offset,
                        close: close_offset,
                        color,
                    }
                })
                .collect();

            self.instance_count = instances.len() as u32;

            if self.instance_buffer.is_none() || self.instance_capacity < instances.len() {
                // Reallocate buffer
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Kline Instance Buffer"),
                    contents: bytemuck::cast_slice(&instances),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                });
                self.instance_buffer = Some(buffer);
                self.instance_capacity = instances.len();
            } else {
                // Update existing buffer
                if let Some(buffer) = &self.instance_buffer {
                    queue.write_buffer(buffer, 0, bytemuck::cast_slice(&instances));
                }
            }
            
            self.last_data_id = Some(current_id);
            self.last_data_len = klines.len();
        }

        if self.instance_count > 0 {
            // 2. Update Uniforms (Every frame, very cheap)
            
            // Use unified coordinate transformation from ChartState
            // This ensures K-lines and Volume Profile use identical transform parameters
            let state = &view_state.state;
            let transform_params = state.compute_transform_params(bounds);
            
            // Update stored base values to match unified system
            self.base_time_ms = transform_params.data_base_time_ms as f64;
            self.base_price_units = transform_params.data_base_price_units;
            
            // Use unified transform parameters
            let transform_x = transform_params.transform_x;
            let transform_y = transform_params.transform_y;
            let transform_z = transform_params.transform_z;
            let transform_w = transform_params.transform_w;

            let uniforms = KlineUniforms {
                transform: [transform_x, transform_y, transform_z, transform_w],
                screen_size: [bounds.width, bounds.height],
                candle_width: state.cell_width * state.scaling,
                _padding: 0.0,
            };

            // Ensure uniform buffer exists
            if self.uniform_buffer.is_none() {
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Kline Uniform Buffer"),
                    contents: bytemuck::cast_slice(&[uniforms]),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });
                self.uniform_buffer = Some(buffer);
                
                // Create Bind Group
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Kline Bind Group"),
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
                // Only update if uniforms actually changed
                // This avoids unnecessary GPU buffer writes during dragging
                let needs_update = self.last_uniforms.as_ref()
                    .map(|last| {
                        last.transform != uniforms.transform
                        || last.screen_size != uniforms.screen_size
                        || last.candle_width != uniforms.candle_width
                    })
                    .unwrap_or(true);
                
                if needs_update {
                    queue.write_buffer(self.uniform_buffer.as_ref().unwrap(), 0, bytemuck::cast_slice(&[uniforms]));
                    self.last_uniforms = Some(uniforms);
                }
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
            render_pass.set_vertex_buffer(0, instance_buffer.slice(..));
            render_pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            render_pass.draw_indexed(0..12, 0, 0..self.instance_count);
        }
    }
}
