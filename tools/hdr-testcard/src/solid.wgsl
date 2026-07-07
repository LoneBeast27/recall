// Solid-colour fill for one scissored viewport region.
//
// The vertex stage emits a fullscreen triangle (no vertex buffer); the region
// actually written is constrained by the CPU-side scissor rect. The fragment
// stage writes the commanded LINEAR value straight out with no clamping and no
// tonemap — the whole point is that the pixel the capture engine reads back
// equals the value we commanded. On an Rgba16Float / scRGB surface a value
// like 2.0 survives verbatim; on an 8-bit sRGB surface the hardware clamps to
// [0,1] on store (which the startup JSON flags as surface="srgb8").

struct Color {
    rgba: vec4<f32>,
};

@group(0) @binding(0) var<uniform> color: Color;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Oversized triangle covering the whole clip volume.
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) & 1) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return color.rgba;
}
