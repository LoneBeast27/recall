// HDR->SDR tonemap for Recall's live output window.
//
// Input: the captured frame as an Rgba16Float texture in scRGB-linear
// (Rec.709 primaries, linear light, where 1.0 == SDR reference white / 80 nits;
// HDR highlights go above 1.0). Output: linear SDR in [0,1] — the swapchain is
// an *-Srgb format, so the hardware applies the linear->sRGB encode on store.
//
// `mode` lets the capture side pick per-frame based on the detected colorspace:
//   0 = passthrough  (source is already SDR -> clamp only, pixel-exact)
//   1 = tonemap      (source is HDR -> compress highlights, preserve midtones)

struct Params {
    mode: u32,
    // scRGB value mapped to SDR white. 1.0 keeps Windows' 80-nit reference;
    // raise it to pull the whole image down if HDR content reads too bright.
    sdr_white: f32,
    // Highlight white point (in SDR-white units) where the rolloff saturates.
    // Larger = more linear midtones, later highlight compression.
    hdr_white: f32,
    _pad0: f32,
    // NDC scale to letterbox the source into the window without distortion.
    scale: vec2<f32>,
    _pad1: vec2<f32>,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_smp: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Fullscreen triangle — no vertex buffer; UVs cover [0,1] across the screen.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    out.uv = uv;
    var ndc = uv * 2.0 - 1.0;
    // Flip Y so texture row 0 lands at the top of the window.
    ndc.y = -ndc.y;
    // Aspect-preserve letterbox: shrink the quad on the over-long axis; the
    // surrounding area keeps the clear color as bars.
    ndc = ndc * params.scale;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    return out;
}

// Knee rolloff: identity below `KNEE`, soft compression of highlights toward
// 1.0 above it (saturating as input approaches `white`). Unlike extended
// Reinhard this leaves midtones/text untouched and only bends bright values.
fn tonemap_channel(x: f32, white: f32) -> f32 {
    let knee = 0.8;
    if (x <= knee) {
        return x;
    }
    let t = (x - knee) / max(white - knee, 1e-3);
    return knee + (1.0 - knee) * (1.0 - exp(-t));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var c = textureSample(src_tex, src_smp, in.uv).rgb;

    if (params.mode == 0u) {
        // Passthrough: clamp wide-gamut/over-range stragglers, keep SDR exact.
        return vec4<f32>(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
    }

    // HDR: normalize to SDR-white units, drop any negative (out-of-gamut) values.
    c = max(c, vec3<f32>(0.0)) / max(params.sdr_white, 1e-4);
    let w = max(params.hdr_white, 1.0);
    let mapped = vec3<f32>(
        tonemap_channel(c.r, w),
        tonemap_channel(c.g, w),
        tonemap_channel(c.b, w),
    );
    return vec4<f32>(clamp(mapped, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
