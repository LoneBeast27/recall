// D3D11 twin of tonemap.wgsl for the zero-copy recording path (gpu_frames.rs).
//
// Input: the WGC capture texture, Rgba16F scRGB-linear (1.0 = SDR white, HDR above 1.0).
// Output: an FFmpeg D3D11 frame (B8G8R8A8_UNORM, NVENC's ARGB input). That texture is not
// typeless, so the linear -> sRGB encode the wgpu path got from an *-Srgb target is done
// here, with the exact piecewise sRGB curve.
//
// Pixel-exact: output (x, y) reads source texel (x, y); outside the source is black. The
// encoder frame keeps its size across a few pixels of window-border jitter, so this crops
// or pads, never scales (same as encoder/frame_fit.rs did on the CPU).

Texture2D<float4> src : register(t0);

cbuffer Params : register(b0) {
    uint mode;        // 0 = passthrough (SDR source), 1 = tonemap (HDR source)
    float sdr_white;  // scRGB value mapped to SDR white
    float hdr_white;  // highlight white point, in SDR-white units
    uint pad0;
    int2 src_size;    // source texels; the target may be a few px larger/smaller
    int2 pad1;
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

float4 ps_main(float4 pos : SV_Position) : SV_Target {
    int2 p = int2(pos.xy);
    if (p.x >= src_size.x || p.y >= src_size.y) {
        return float4(0.0, 0.0, 0.0, 1.0);
    }
    float3 c = src.Load(int3(p, 0)).rgb;
    if (mode == 0) {
        c = saturate(c);
    } else {
        c = max(c, 0.0) / max(sdr_white, 1e-4);
        float w = max(hdr_white, 1.0);
        c = saturate(float3(tonemap_channel(c.r, w), tonemap_channel(c.g, w), tonemap_channel(c.b, w)));
    }
    return float4(srgb_encode(c.r), srgb_encode(c.g), srgb_encode(c.b), 1.0);
}
