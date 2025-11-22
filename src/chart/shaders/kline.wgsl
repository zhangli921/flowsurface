struct Uniforms {
    projection: mat4x4<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct InstanceInput {
    @location(0) x: f32,
    @location(1) y_open: f32,
    @location(2) y_high: f32,
    @location(3) y_low: f32,
    @location(4) y_close: f32,
    @location(5) width: f32,
    @location(6) color: vec4<f32>,
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
    
    // In screen coordinates (Y down):
    // High price -> Low Y (Top)
    // Low price -> High Y (Bottom)
    
    // Body Y range
    var y_top_body = min(instance.y_open, instance.y_close);
    var y_bottom_body = max(instance.y_open, instance.y_close);
    
    // Minimum thickness for body
    if (y_bottom_body - y_top_body < 1.0) {
        y_bottom_body = y_top_body + 1.0;
    }
    
    // Wick Y range (y_high is top/min Y, y_low is bottom/max Y)
    var y_top_wick = instance.y_high;
    var y_bottom_wick = instance.y_low;
    
    let half_width = instance.width / 2.0;
    let half_wick = 0.5; // 1px width wick
    
    var x_offset: f32;
    var y_pos: f32;
    
    // Indices 0-3: Body
    // Indices 4-7: Wick
    
    if (in_vertex_index < 4u) {
        // Body Quad
        // 0: TL (-w, top)
        // 1: TR (+w, top)
        // 2: BR (+w, bottom)
        // 3: BL (-w, bottom)
        
        let is_right = (in_vertex_index == 1u || in_vertex_index == 2u);
        let is_bottom = (in_vertex_index == 2u || in_vertex_index == 3u);
        
        x_offset = select(-half_width, half_width, is_right);
        y_pos = select(y_top_body, y_bottom_body, is_bottom);
    } else {
        // Wick Quad
        // 4: TL (-w_wick, top_wick)
        // 5: TR (+w_wick, top_wick)
        // 6: BR (+w_wick, bottom_wick)
        // 7: BL (-w_wick, bottom_wick)
        
        let is_right = (in_vertex_index == 5u || in_vertex_index == 6u);
        let is_bottom = (in_vertex_index == 6u || in_vertex_index == 7u);
        
        x_offset = select(-half_wick, half_wick, is_right);
        y_pos = select(y_top_wick, y_bottom_wick, is_bottom);
    }
    
    let final_x = instance.x + x_offset;
    let final_y = y_pos;
    
    out.clip_position = uniforms.projection * vec4<f32>(final_x, final_y, 0.0, 1.0);
    out.color = instance.color;
    
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}

