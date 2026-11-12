// AMD FidelityFX Super Resolution 1 (EASU upscale + RCAS sharpen) as two compute passes.
// ffx_a.h and ffx_fsr1.h are AMD's MIT-licensed sources (LICENSE-FidelityFX-FSR.txt);
// fsr1.rs places them where the two markers below are before compiling.
// Runs on any D3D11 feature level 11_0 GPU (cs_5_0): GTX 10 series and up.

cbuffer Sizes : register(b0) {
    float4 sizes;   // input width, input height, output width, output height
    float4 tuning;  // x: RCAS sharpness in stops (0 = most)
};

#define A_GPU 1
#define A_HLSL 1
//@@FFX_A@@

Texture2D<float4> Source : register(t0);
SamplerState LinearClamp : register(s0);
RWTexture2D<float4> Target : register(u0);

#define FSR_EASU_F 1
AF4 FsrEasuRF(AF2 p) { return Source.GatherRed(LinearClamp, p); }
AF4 FsrEasuGF(AF2 p) { return Source.GatherGreen(LinearClamp, p); }
AF4 FsrEasuBF(AF2 p) { return Source.GatherBlue(LinearClamp, p); }

#define FSR_RCAS_F 1
// Clamp to the frame: outside it Load returns black, which would sharpen the border against it.
AF4 FsrRcasLoadF(ASU2 p) { return Source.Load(int3(clamp(p, ASU2(0, 0), ASU2(sizes.zw) - 1), 0)); }
void FsrRcasInputF(inout AF1 r, inout AF1 g, inout AF1 b) {}

//@@FFX_FSR1@@

[numthreads(8, 8, 1)]
void easu(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)sizes.z || id.y >= (uint)sizes.w) return;
    AU4 c0, c1, c2, c3;
    FsrEasuCon(c0, c1, c2, c3, sizes.x, sizes.y, sizes.x, sizes.y, sizes.z, sizes.w);
    AF3 colour;
    FsrEasuF(colour, id.xy, c0, c1, c2, c3);
    Target[id.xy] = float4(colour, 1.0);
}

[numthreads(8, 8, 1)]
void rcas(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)sizes.z || id.y >= (uint)sizes.w) return;
    AU4 con;
    FsrRcasCon(con, tuning.x);
    AF1 r, g, b;
    FsrRcasF(r, g, b, id.xy, con);
    Target[id.xy] = float4(r, g, b, 1.0);
}
