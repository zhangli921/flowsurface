// Session Volume Profile (S-VP) Instanced Renderer Shader
// This shader renders volume profile bars as horizontal bars on the chart.

struct Uniforms {
    // Projection matrix to transform from chart space to clip space
    projection: mat4x4<f32>,
    // Chart viewport parameters (might be unused if we pass pos directly)
    chart_min_price: f32,
    chart_max_price: f32,
    chart_min_x: f32,
    chart_max_x: f32,
    // SVP rendering parameters
    svp_x_offset: f32,
    svp_max_width: f32,
    price_tick_size: f32,
    _padding: f32,
}

struct InstanceInput {
    // Pre-calculated position and size in chart coordinates (pixels/logical units)
    @location(0) position: vec2<f32>, 
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>, // Pass color directly for flexibility
}

struct VertexInput {
    @location(3) position: vec2<f32>,  // Base quad vertex position [0,0] to [1,1]
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@vertex
fn vs_main(
    vertex: VertexInput,
    instance: InstanceInput,
) -> VertexOutput {
    var out: VertexOutput;
    
    // Basic quad expansion: pos + vertex_pos * size
    // vertex.position is [0,0] to [1,1]
    let pos_x = instance.position.x + vertex.position.x * instance.size.x;
    let pos_y = instance.position.y + vertex.position.y * instance.size.y;
    
    // Apply projection matrix (transforms chart space -> clip space)
    let world_pos = vec4<f32>(pos_x, pos_y, 0.0, 1.0);
    out.clip_position = uniforms.projection * world_pos;
    
    out.color = instance.color;
    
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
