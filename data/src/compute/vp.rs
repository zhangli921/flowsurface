//! GPGPU compute pipeline for calculating Session Volume Profile (S-VP).

use std::sync::Arc;
use bytemuck::{self, Pod, Zeroable};
use crate::data_error::DataError;
use wgpu::{self, util::DeviceExt, BindingType, BufferBindingType, MapMode};
use tokio::sync::oneshot;
use thiserror::Error;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct ComputeParams {
    pub num_ticks: u32,
    pub price_resolution: u32,
    pub min_price: u32,
    pub volume_scaling_factor: u32,
    pub tick_offset: u32, // Offset for chunked processing
}

// TickDataBuffer to match the shader's input structure
pub struct TickDataBuffer {
    pub prices: Vec<u32>,  // Fixed-point price * 100, using u32 instead of u64 for WGSL compatibility
    pub volumes: Vec<f32>,
    // Time range covered by the data (for validation)
    pub time_range: Option<(u64, u64)>, // (start_us, end_us) - None if timestamps not tracked
}

#[derive(Debug, Clone, Copy)]
pub struct SparseBar {
    pub price_level: u32,
    pub volume: u32,
}

#[derive(Debug, Clone)]
pub struct VolumeProfile {
    pub bars: Arc<Vec<SparseBar>>,
    pub point_of_control: u32,
    pub value_area_start: u32,
    pub value_area_end: u32,
}

#[derive(Error, Debug, Clone)]
pub enum ComputeError {
    #[error("Failed to map GPU buffer")]
    MapError,
    #[error("Channel closed unexpectedly")]
    ChannelClosed,
    #[error("Data error: {0}")]
    DataError(String),
    #[error("Compute error: {0}")]
    Other(String),
}

impl From<DataError> for ComputeError {
    fn from(err: DataError) -> Self {
        ComputeError::DataError(err.to_string())
    }
}

/// Manages WGPU resources for the Volume Profile compute shader.
#[derive(Debug)]
pub struct VpComputePipeline {
    pub compute_pipeline: wgpu::ComputePipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
}

impl VpComputePipeline {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader_source = include_str!("vp_compute.wgsl");
        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("VP Compute Shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("VP Bind Group Layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: BindingType::Buffer {
                            ty: BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: BindingType::Buffer {
                            ty: BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: BindingType::Buffer {
                            ty: BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: BindingType::Buffer {
                            ty: BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("VP Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("VP Compute Pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader_module,
            entry_point: "main",
        });

        Self {
            compute_pipeline,
            bind_group_layout,
        }
    }

    pub async fn run_aggregation(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ticks: &TickDataBuffer,
        params: &ComputeParams,
        histogram_buckets: u64,
    ) -> Result<VolumeProfile, ComputeError> {
        let histogram_buffer_size = histogram_buckets * std::mem::size_of::<u32>() as u64;
        
        let price_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("VP Price Buffer"),
            contents: bytemuck::cast_slice(&ticks.prices),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let volume_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("VP Volume Buffer"),
            contents: bytemuck::cast_slice(&ticks.volumes),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("VP Uniform Buffer"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        
        let histogram_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("VP Histogram Buffer"),
            size: histogram_buffer_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor { 
            label: Some("VP Staging Buffer"),
            size: histogram_buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("VP Bind Group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: price_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: volume_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: histogram_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: uniform_buffer.as_entire_binding() },
            ],
        });

        let workgroup_size = 64;
        let total_workgroups = (params.num_ticks + workgroup_size - 1) / workgroup_size;
        
        // WGPU limits: Each dispatch group size dimension must be <= 65535
        const MAX_WORKGROUPS_PER_DISPATCH: u32 = 65535;
        
        // Pre-create all uniform buffers and bind groups if we need to split
        let mut chunk_uniform_buffers = Vec::new();
        let mut chunk_bind_groups = Vec::new();
        let mut chunk_workgroups = Vec::new();
        
        if total_workgroups > MAX_WORKGROUPS_PER_DISPATCH {
            // Split into multiple dispatches with offset support
            let mut offset = 0u32;
            let mut remaining_ticks = params.num_ticks;
            
            while remaining_ticks > 0 {
                let workgroups_this_dispatch = (remaining_ticks + workgroup_size - 1) / workgroup_size;
                let workgroups_this_dispatch = workgroups_this_dispatch.min(MAX_WORKGROUPS_PER_DISPATCH);
                let ticks_this_dispatch = workgroups_this_dispatch * workgroup_size;
                let ticks_this_dispatch = ticks_this_dispatch.min(remaining_ticks);
                
                // Create a new uniform buffer with updated offset
                let chunk_params = ComputeParams {
                    num_ticks: params.num_ticks, // Keep original total for boundary check
                    price_resolution: params.price_resolution,
                    min_price: params.min_price,
                    volume_scaling_factor: params.volume_scaling_factor,
                    tick_offset: offset,
                };
                
                let chunk_uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("VP Uniform Buffer (Chunk)"),
                    contents: bytemuck::bytes_of(&chunk_params),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                
                let chunk_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("VP Bind Group (Chunk)"),
                    layout: &self.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: price_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: volume_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: histogram_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: chunk_uniform_buffer.as_entire_binding() },
                    ],
                });
                
                chunk_uniform_buffers.push(chunk_uniform_buffer);
                chunk_bind_groups.push(chunk_bind_group);
                chunk_workgroups.push(workgroups_this_dispatch);
                
                offset += ticks_this_dispatch;
                remaining_ticks = remaining_ticks.saturating_sub(ticks_this_dispatch);
                
                if remaining_ticks == 0 {
                    break;
                }
            }
            
            log::debug!(
                "VP computation: Split {} ticks into {} dispatches",
                params.num_ticks,
                chunk_bind_groups.len()
            );
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("VP Command Encoder"),
        });

        // Zero out the histogram buffer before use
        encoder.clear_buffer(&histogram_buffer, 0, None);

        {
            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("VP Compute Pass"),
                timestamp_writes: None,
            });
            compute_pass.set_pipeline(&self.compute_pipeline);
            
            if total_workgroups <= MAX_WORKGROUPS_PER_DISPATCH {
                // Single dispatch is sufficient
                compute_pass.set_bind_group(0, &bind_group, &[]);
                compute_pass.dispatch_workgroups(total_workgroups, 1, 1);
            } else {
                // Dispatch all chunks (bind groups are kept alive by the vectors)
                for (bind_group, workgroups) in chunk_bind_groups.iter().zip(chunk_workgroups.iter()) {
                    compute_pass.set_bind_group(0, bind_group, &[]);
                    compute_pass.dispatch_workgroups(*workgroups, 1, 1);
                }
            }
        }

        encoder.copy_buffer_to_buffer(&histogram_buffer, 0, &staging_buffer, 0, histogram_buffer_size);
        queue.submit(Some(encoder.finish()));

        // Use tokio::sync::oneshot to bridge the map_async callback
        let (sender, receiver) = oneshot::channel();
        let s_params = params.clone();

        // Wrap staging_buffer in an Arc, so we can share it with the async callback
        let staging_buffer_arc = std::sync::Arc::new(staging_buffer);
        // Clone the Arc for the callback. The callback will take ownership of the clone.
        let staging_buffer_for_callback = staging_buffer_arc.clone();

        // Get the slice from the original Arc
        let buffer_slice = staging_buffer_arc.slice(..);

        // map_async will take ownership of the closure, so we move the cloned Arc into it.
        buffer_slice.map_async(MapMode::Read, move |map_result| {
            let _ = sender.send(match map_result {
                Ok(()) => {
                    let data: Vec<u32> = {
                        // Use the cloned Arc inside the callback
                        let view = staging_buffer_for_callback.slice(..).get_mapped_range();
                        let result = bytemuck::cast_slice(&view).to_vec();
                        drop(view); // Unmap the buffer
                        result
                    };
                    Ok(process_histogram(data, s_params.min_price, s_params.price_resolution))
                },
                Err(_e) => Err(ComputeError::MapError),
            });
        });

        // IMPORTANT: Poll the device to ensure the map_async callback is executed
        device.poll(wgpu::Maintain::Wait);

        let compute_result = receiver.await.map_err(|_| ComputeError::ChannelClosed)??;
        Ok(compute_result)
    }
}

fn process_histogram(dense_histogram: Vec<u32>, min_price: u32, price_resolution: u32) -> VolumeProfile {
    if dense_histogram.is_empty() {
        return VolumeProfile {
            bars: Arc::new(Vec::new()),
            point_of_control: 0,
            value_area_start: 0,
            value_area_end: 0,
        };
    }

    let mut total_volume: u64 = 0;
    let mut point_of_control_volume: u32 = 0;
    let mut point_of_control_price: u32 = 0;

    let bars: Vec<SparseBar> = dense_histogram
        .into_iter()
        .enumerate()
        .filter_map(|(i, volume)| {
            if volume > 0 {
                let price_level = min_price + (i as u32 * price_resolution);
                total_volume += volume as u64;

                if volume > point_of_control_volume {
                    point_of_control_volume = volume;
                    point_of_control_price = price_level;
                }

                Some(SparseBar { price_level, volume })
            } else {
                None
            }
        })
        .collect();

    if bars.is_empty() {
        return VolumeProfile {
            bars: Arc::new(bars),
            point_of_control: 0,
            value_area_start: 0,
            value_area_end: 0,
        };
    }

    // Calculate Value Area (70% of volume)
    let value_area_volume_target = (total_volume as f64 * 0.7) as u64;
    let poc_index = bars.iter().position(|b| b.price_level == point_of_control_price).unwrap_or(0);

    let mut current_va_volume = point_of_control_volume as u64;
    let mut low_index = poc_index as i32 - 1;
    let mut high_index = poc_index + 1;
    let mut value_area_start = point_of_control_price;
    let mut value_area_end = point_of_control_price;
    
    while current_va_volume < value_area_volume_target {
        let low_bar_volume = if low_index >= 0 { bars.get(low_index as usize).map_or(0, |b| b.volume) } else { 0 };
        let high_bar_volume = bars.get(high_index).map_or(0, |b| b.volume);

        if low_bar_volume == 0 && high_bar_volume == 0 {
            break; // No more bars to add
        }

        if low_bar_volume > high_bar_volume {
            current_va_volume += low_bar_volume as u64;
            value_area_start = bars[low_index as usize].price_level;
            low_index -= 1;
        } else {
            current_va_volume += high_bar_volume as u64;
            value_area_end = bars[high_index].price_level;
            high_index += 1;
        }
    }

    VolumeProfile {
        bars: Arc::new(bars),
        point_of_control: point_of_control_price,
        value_area_start,
        value_area_end,
    }
}