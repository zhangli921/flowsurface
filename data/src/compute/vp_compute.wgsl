// GPGPU compute shader for calculating Session Volume Profile (S-VP)

// Input buffers containing tick data (prices and volumes)
// Prices are stored as u64 to avoid floating point precision issues.
// They represent the price multiplied by a scaling factor (e.g., 100 for 2 decimal places).
@group(0) @binding(0) var<storage, read> prices: array<u64>;
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
};

@group(0) @binding(3) var<uniform> compute_params: ComputeParams;


[[stage(compute), workgroup_size(64, 1, 1)]]
fn main([[builtin(global_invocation_id)]] global_id: vec3<u32>) {
    let tick_index = global_id.x;

    // Boundary check
    if (tick_index >= compute_params.num_ticks) {
        return;
    }

    // 1. Read price and volume
    let price_raw = u32(prices[tick_index]);
    let volume_f32 = volumes[tick_index];

    // 2. Price Binning
    // Map the raw price to an index in our histogram array.
    let price_index = (price_raw - compute_params.min_price) / compute_params.price_resolution;

    // Boundary check for histogram array to prevent out-of-bounds access
    // This check is important as price_index could be negative if price_raw < compute_params.min_price
    // but WGSL u32 prevents that. It could also be very large if price_raw is large.
    if (price_index >= arrayLength(&histogram)) {
        return;
    }

    // 3. Volume Scaling and Atomic Accumulation
    // Convert f32 volume to a scaled u32 to use with atomicAdd.
    let volume_u32 = u32(volume_f32 * f32(compute_params.volume_scaling_factor));
    atomicAdd(&histogram[price_index], volume_u32);
}

