# GPU features by NVIDIA generation

Recall supports every NVIDIA card from the GTX 10 series up. A feature that some cards lack
must step down to something every card runs; it is never hidden on older cards. Update this
table whenever a feature starts depending on the GPU.

| Feature | GTX 10 (Pascal) | GTX 16 (Turing, no RTX) | RTX 20 (Turing) | RTX 30 (Ampere) | RTX 40 (Ada) | RTX 50 (Blackwell) | When missing |
|---|---|---|---|---|---|---|---|
| Record H.264 (NVENC) | yes | yes | yes | yes | yes | yes | n/a |
| Record HEVC (NVENC) | yes | yes¹ | yes | yes | yes | yes | n/a |
| Record AV1 (NVENC) | no | no | no | no | yes | yes | records H.264 (encoder fallback chain) |
| Export above 4096 px a side: HEVC up to 8192 | yes | yes | yes | yes | yes | yes | n/a (H.264 stops at 4096, so larger exports are HEVC) |
| GPU export: decode H.264 / HEVC (D3D11VA) | yes | yes | yes | yes | yes | yes | n/a |
| GPU export: decode AV1 (D3D11VA) | no | no | no | yes | yes | yes | export decodes on the CPU |
| Upscaled export: RTX Video Super Resolution (AI) | no | no | yes² | yes² | yes² | yes² | FSR 1 |
| Upscaled export: AMD FSR 1 (shader, cs_5_0) | yes | yes | yes | yes | yes | yes | plain video processor scale |
| Upscaled export: plain video processor scale | yes | yes | yes | yes | yes | yes | n/a |

The GT 1030 and the MX150 to MX350 laptop chips have no NVENC, so they sit below the GTX 10 floor: Recall cannot record on them at all.

¹ The GTX 1650's NVENC depends on the variant: the first GDDR5 cards have the older (Pascal-generation) encoder,
which has no HEVC B-frames; GDDR6 and SUPER cards have the Turing encoder. Neither has AV1. Recall records and exports
without B-frames, so both kinds record HEVC.

² Only for pictures of 640x360 or more (NVIDIA's model range; a smaller clip gets FSR 1). Needs the driver switch: NVIDIA app > System > Video > RTX Video Enhancement > Super
resolution. Recall never changes it. When it is off, the export uses FSR 1 and the export page
tells the user where to turn it on.

## How the upscaler is picked (`recall/src/export/upscale/mod.rs`, `pick`)

1. The vendor's AI super resolution, if the driver really applies it. Recall upscales a test
   pattern with it on and off and compares the pixels, so a card without it or with the
   switch off counts as "not ready".
2. Otherwise FSR 1, which runs on any D3D11 feature level 11_0 GPU.
3. Otherwise the D3D11 video processor's own scale.

`RECALL_UPSCALER=ai|fsr|plain` forces a tier for checks.

## Sources (read 2026-11-12)

- NVENC and NVDEC per generation: NVIDIA, "Video Encode and Decode GPU Support Matrix",
  https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new
- RTX Video Super Resolution on RTX 20 series and newer (needs Tensor cores): NVIDIA RTX Video
  SDK, https://developer.nvidia.com/rtx-video-sdk/getting-started
- FSR 1: AMD GPUOpen FidelityFX-FSR, MIT licence (`recall/src/export/upscale/fsr1/`)

No export-path timing was taken for either upscaler; the GPU export tests check that each one changes the picture.
No GTX 10 or 16 card was available to test FSR 1 on; it needs compute shader model 5.0, which GTX 10 cards have.
