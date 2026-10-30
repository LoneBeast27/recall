// D3D11 twin of tonemap.wgsl for the zero-copy recording path (gpu_frames.rs).
//
// Input: the WGC capture texture, Rgba16F scRGB-linear (1.0 = SDR white, HDR above 1.0).
// Output: an FFmpeg D3D11 frame (B8G8R8A8_UNORM, NVENC's ARGB input). That texture is not
// typeless, so the linear -> sRGB encode the wgpu path got from an *-Srgb target is done
// here, with the exact piecewise sRGB curve.
//
// Placement (framing.rs `Layout`): the `src_rect` texels land in `dst_rect` of the output;
// everything outside `dst_rect` is black. Equal sizes are a pixel-exact copy (output
// (x, y) reads texel src.xy + (x, y) - dst.xy): window-border jitter is cropped or padded,
// and a windowed game's client area is cut out of its frame and centred on the canvas.
// Different sizes (a client area larger than the canvas) are a bilinear resize, sampled
// only inside `src_rect` so no title-bar texel bleeds in.

Texture2D<float4> src : register(t0);
SamplerState smp : register(s0);

cbuffer Params : register(b0) {
    uint mode;        // 0 = passthrough (SDR source), 1 = tonemap (HDR source)
    float sdr_white;  // scRGB value mapped to SDR white
    float hdr_white;  // highlight white point, in SDR-white units
    uint scaled;      // 0 = pixel-exact copy, 1 = filtered resize
    int4 src_rect;    // x, y, w, h in source texels
    int4 dst_rect;    // x, y, w, h in output pixels
};

float4 vs_main(uint vi : SV_VertexID) : SV_Position {
    float2 uv = float2((vi << 1) & 2, vi & 2);
    return float4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

float tonemap_channel(float x, float white) {
    const float knee = 0.8;
    if (x <= knee) {
        return x;
    }
    float t = (x - knee) / max(white - knee, 1e-3);
    return knee + (1.0 - knee) * (1.0 - exp(-t));
}

float srgb_encode(float c) {
    return c <= 0.0031308 ? 12.92 * c : 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

float3 source_at(int2 p) {
    if (scaled == 0) {
        return src.Load(int3(src_rect.xy + p, 0)).rgb;
    }
    uint tw, th;
    src.GetDimensions(tw, th);
    float2 lo = float2(src_rect.xy) + 0.5;
    float2 hi = float2(src_rect.xy + src_rect.zw) - 0.5;
    float2 at = float2(src_rect.xy) + (float2(p) + 0.5) * float2(src_rect.zw) / float2(dst_rect.zw);
    return src.SampleLevel(smp, clamp(at, lo, hi) / float2(tw, th), 0).rgb;
}

float4 ps_main(float4 pos : SV_Position) : SV_Target {
    int2 p = int2(pos.xy) - dst_rect.xy;
    if (p.x < 0 || p.y < 0 || p.x >= dst_rect.z || p.y >= dst_rect.w) {
        return float4(0.0, 0.0, 0.0, 1.0);
    }
    float3 c = source_at(p);
    if (mode == 0) {
        c = saturate(c);
    } else {
        c = max(c, 0.0) / max(sdr_white, 1e-4);
        float w = max(hdr_white, 1.0);
        c = saturate(float3(tonemap_channel(c.r, w), tonemap_channel(c.g, w), tonemap_channel(c.b, w)));
    }
    return float4(srgb_encode(c.r), srgb_encode(c.g), srgb_encode(c.b), 1.0);
}
