struct Uniforms {
    // x: unused, y: unused, z: price_scale, w: price_offset
    transform: vec4<f32>,
    // x: screen_width, y: screen_height
    screen_size: vec2<f32>,
    // x: svp_x_start, y: volume_scale
    svp_params: vec2<f32>,
    bar_height: f32,
    _padding: vec3<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct InstanceInput {
    @location(0) price: f32,
    @location(1) volume: f32,
    @location(2) color: vec4<f32>,
}

struct VertexInput {
    @location(3) position: vec2<f32>, // [0,0] to [1,1]
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(
    vertex: VertexInput,
    instance: InstanceInput,
) -> VertexOutput {
    var out: VertexOutput;

    // Y Position (Price -> Screen Y)
    // High price -> Low Y (Top). transform.z is negative usually or we flip here.
    // Assuming transform handles mapping logic.
    let center_y = (instance.price * uniforms.transform.z) + uniforms.transform.w;
    
    // Bar Layout
    // Width based on volume
    let width = instance.volume * uniforms.svp_params.y;
    
    // X Position: Start at svp_params.x, extend right
    let pos_x = uniforms.svp_params.x + (vertex.position.x * width);
    
    // Y Position: Centered at price, fixed height
    let pos_y = (center_y - uniforms.bar_height / 2.0) + (vertex.position.y * uniforms.bar_height);

    // Clip Space Conversion
    let clip_x = (pos_x / uniforms.screen_size.x) * 2.0 - 1.0;
    let clip_y = -((pos_y / uniforms.screen_size.y) * 2.0 - 1.0);

    out.clip_position = vec4<f32>(clip_x, clip_y, 0.0, 1.0);
    out.color = instance.color;

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
