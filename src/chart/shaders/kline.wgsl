struct Uniforms {
    // x: time_scale, y: time_offset, z: price_scale, w: price_offset
    transform: vec4<f32>,
    // x: screen_width, y: screen_height
    screen_size: vec2<f32>,
    candle_width: f32,
    _padding: f32,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct InstanceInput {
    @location(0) time_offset: f32,
    @location(1) open: f32,
    @location(2) high: f32,
    @location(3) low: f32,
    @location(4) close: f32,
    @location(5) color: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(
    @builtin(vertex_index) in_vertex_index: u32,
    instance: InstanceInput,
) -> VertexOutput {
    var out: VertexOutput;

    // 1. Physics -> Screen Coordinates
    // X = (time_offset * scale_x) + offset_x
    let center_x = (instance.time_offset * uniforms.transform.x) + uniforms.transform.y;
    
    // Y = (price * scale_y) + offset_y
    // Prices are f32. High price should be lower Y (top of screen).
    let y_open = (instance.open * uniforms.transform.z) + uniforms.transform.w;
    let y_close = (instance.close * uniforms.transform.z) + uniforms.transform.w;
    let y_high = (instance.high * uniforms.transform.z) + uniforms.transform.w;
    let y_low = (instance.low * uniforms.transform.z) + uniforms.transform.w;

    // 2. Body Bounds
    var y_top_body = min(y_open, y_close);
    var y_bottom_body = max(y_open, y_close);

    // Minimum thickness 1px
    if (y_bottom_body - y_top_body < 1.0) {
        y_bottom_body = y_top_body + 1.0;
    }

    // 3. Wick Bounds
    let y_top_wick = min(y_high, y_low);
    let y_bottom_wick = max(y_high, y_low);

    let half_width = uniforms.candle_width / 2.0;
    let half_wick = 0.5; 

    var x_offset: f32;
    var y_pos: f32;

    if (in_vertex_index < 4u) {
        // Body Quad
        let is_right = (in_vertex_index == 1u || in_vertex_index == 2u);
        let is_bottom = (in_vertex_index == 2u || in_vertex_index == 3u);
        x_offset = select(-half_width, half_width, is_right);
        y_pos = select(y_top_body, y_bottom_body, is_bottom);
    } else {
        // Wick Quad
        let is_right = (in_vertex_index == 5u || in_vertex_index == 6u);
        let is_bottom = (in_vertex_index == 6u || in_vertex_index == 7u);
        x_offset = select(-half_wick, half_wick, is_right);
        y_pos = select(y_top_wick, y_bottom_wick, is_bottom);
    }

    let final_x = center_x + x_offset;
    let final_y = y_pos;

    // 4. Clip Space
    let clip_x = (final_x / uniforms.screen_size.x) * 2.0 - 1.0;
    let clip_y = -((final_y / uniforms.screen_size.y) * 2.0 - 1.0); 

    out.clip_position = vec4<f32>(clip_x, clip_y, 0.0, 1.0);
    out.color = instance.color;

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
