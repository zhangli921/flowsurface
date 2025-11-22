//! Service for managing a headless WGPU context dedicated to compute tasks.

use crate::compute::vp::{ComputeError, ComputeParams, TickDataBuffer, VolumeProfile, VpComputePipeline};
use wgpu::{self, Instance, RequestAdapterOptions, PowerPreference};
use std::sync::Arc;

#[derive(Debug)]
pub struct VpComputeService {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    pipeline: VpComputePipeline,
}

impl VpComputeService {
    pub async fn new() -> Result<Self, String> {
        log::info!("VpComputeService: initializing...");
        let instance = Instance::new(wgpu::InstanceDescriptor::default());

        log::info!("VpComputeService: requesting adapter...");
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .ok_or_else(|| "Failed to find an appropriate WGPU adapter".to_string())?;

        log::info!("VpComputeService: requesting device...");
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("VP Compute Device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    ..Default::default()
                },
                None,  // trace_path
            )
            .await
            .map_err(|e: wgpu::RequestDeviceError| format!("Failed to create WGPU device: {}", e))?;

        log::info!("VpComputeService: device created. creating pipeline...");
        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let pipeline = VpComputePipeline::new(&device);

        log::info!("VpComputeService: initialization complete.");
        Ok(Self {
            device,
            queue,
            pipeline,
        })
    }

    pub async fn compute_vp(
        &self,
        ticks: &TickDataBuffer,
        params: &ComputeParams,
        histogram_buckets: u64,
    ) -> Result<VolumeProfile, ComputeError> {
        self.pipeline
            .run_aggregation(&self.device, &self.queue, ticks, params, histogram_buckets)
            .await
    }
}

