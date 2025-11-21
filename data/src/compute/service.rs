use crate::compute::vp::{ComputeError, ComputeParams, TickDataBuffer, VolumeProfile, VpComputePipeline};
use iced::wgpu::{self, Instance, RequestAdapterOptions, PowerPreference};
use std::sync::Arc;

/// A service that manages the WGPU context and pipeline for Volume Profile computation.
/// It runs in a headless environment, independent of the UI rendering context.
#[derive(Debug)]
pub struct VpComputeService {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    pipeline: VpComputePipeline,
}

impl VpComputeService {
    /// Initializes a new VpComputeService with its own WGPU context.
    pub async fn new() -> Result<Self, String> {
        let instance = Instance::new(&wgpu::InstanceDescriptor::default());

        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .map_err(|_| "Failed to find an appropriate WGPU adapter".to_string())?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("VP Compute Device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    ..Default::default()
                }
            )
            .await
            .map_err(|e: wgpu::RequestDeviceError| format!("Failed to create WGPU device: {}", e))?;

        let device = Arc::new(device);
        let queue = Arc::new(queue);
        let pipeline = VpComputePipeline::new(&device);

        Ok(Self {
            device,
            queue,
            pipeline,
        })
    }

    /// Runs the Volume Profile aggregation task.
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

