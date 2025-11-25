// GPGPU compute shader for calculating Session Volume Profile (S-VP)

// Input buffers containing tick data (prices and volumes)
// Prices are stored as u32 (fixed-point) to be compatible with standard WGSL.
// They represent the price multiplied by a scaling factor (e.g., 100 for 2 decimal places).
@group(0) @binding(0) var<storage, read> prices: array<u32>;
@group(0) @binding(1) var<storage, read> volumes: array<f32>;

// Output buffer for the volume histogram. The index represents the price bucket.
@group(0) @binding(2) var<storage, read_write> histogram: array<atomic<u32>>;

// Uniforms providing metadata for the computation
struct ComputeParams {
    num_ticks: u32,
    // The number of price levels per histogram bucket (e.g., 5 ticks per bucket)
    price_resolution: u32, 
    // The minimum price level for the histogram, used as an offset.
    min_price: u32,
    // Factor to scale f32 volume to u32 for atomic operations
    volume_scaling_factor: u32,
    // Offset for chunked processing (used when splitting large datasets)
    tick_offset: u32,
};

@group(0) @binding(3) var<uniform> compute_params: ComputeParams;


@compute @workgroup_size(64, 1, 1)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    // Calculate actual tick index with offset for chunked processing
    let tick_index = global_id.x + compute_params.tick_offset;

    // Boundary check
    if (tick_index >= compute_params.num_ticks) {
        return;
    }

    // 1. Read price and volume
    let price_raw = prices[tick_index];
    let volume_f32 = volumes[tick_index];

    // 2. Price Binning
    // Map the raw price to an index in our histogram array.
    // Note: price_raw and min_price are both u32. Ensure (price_raw - min_price) doesn't underflow logic
    // (though in Rust side we should ensure min_price <= all prices).
    if (price_raw < compute_params.min_price) {
        return;
    }
    let price_index = (price_raw - compute_params.min_price) / compute_params.price_resolution;

    // Boundary check for histogram array to prevent out-of-bounds access
    if (price_index >= arrayLength(&histogram)) {
        return;
    }

    // 3. Volume Scaling and Atomic Accumulation
    // Convert f32 volume to a scaled u32 to use with atomicAdd.
    let volume_u32 = u32(volume_f32 * f32(compute_params.volume_scaling_factor));
    atomicAdd(&histogram[price_index], volume_u32);
}
