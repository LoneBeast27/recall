// One separable pass of the export's GPU downscale: a cubic filter (B = 0, C = 0.6, as
// swscale's default bicubic) along one axis of one plane, widened by the scale ratio so a
// large reduction averages every source texel it covers instead of skipping them.

cbuffer Params : register(b0) {
    float ratio;      // source length / output length along the axis (>= 1 when shrinking)
    float support;    // kernel half-width in source texels
    int horizontal;   // 1: along x, 0: along y
    int cosited;      // 1: 4:2:0 chroma along x, sampled on even luma columns (MPEG-2 / H.264)
    int src_len;      // source length along the axis, in texels of this plane
    int3 pad;
};

Texture2D<float4> src : register(t0);

struct VsOut { float4 pos : SV_Position; };

VsOut vs_main(uint id : SV_VertexID) {
    VsOut o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}

float cubic(float x) {
    const float B = 0.0, C = 0.6;
    x = abs(x);
    if (x < 1.0) return ((12 - 9 * B - 6 * C) * x * x * x + (-18 + 12 * B + 6 * C) * x * x + (6 - 2 * B)) / 6;
    if (x < 2.0) return ((-B - 6 * C) * x * x * x + (6 * B + 30 * C) * x * x + (-12 * B - 48 * C) * x + (8 * B + 24 * C)) / 6;
    return 0;
}

float4 ps_main(VsOut i) : SV_Target {
    int2 at = int2(i.pos.xy);
    float o = horizontal ? at.x : at.y;
    // Source position of this output texel's centre. Cosited chroma: sample k sits on luma
    // column 2k, so map through luma coordinates.
    float centre = cosited ? (((2 * o + 0.5) * ratio - 0.5) / 2) : ((o + 0.5) * ratio - 0.5);
    float widen = max(ratio, 1.0);
    int first = (int)floor(centre - support) + 1;
    int last = (int)floor(centre + support);
    float4 sum = 0;
    float weight = 0;
    [loop] for (int k = first; k <= last; k++) {
        float w = cubic((k - centre) / widen);
        int t = clamp(k, 0, src_len - 1);
        int2 p = horizontal ? int2(t, at.y) : int2(at.x, t);
        sum += w * src.Load(int3(p, 0));
        weight += w;
    }
    return saturate(sum / weight);
}
