# Minimal FFmpeg for Recall (NVIDIA on Windows): plan

Status: plan only, not built. Today Recall ships the Gyan 7.1.1 "full" shared build: 158 MB of DLLs
(avcodec 89 MB, avfilter 42 MB, avformat 19 MB, avdevice 4.5 MB, ...). Rohan, 2026-11-04: this package
is NVIDIA on Windows only. AMD, Intel and Mac get separate packages with their own FFmpeg build.

## What Recall really uses (from the code, 2026-11-04, master 271c179)

| Kind | Engine (ffmpeg-next, in process) | App (ffmpeg.exe / ffprobe.exe children) |
|---|---|---|
| Encoders | h264_nvenc, hevc_nvenc, av1_nvenc (recording); libx264 (export CPU fallback); aac; flac (editor stems); mjpeg (recorder thumbnail) | hevc_nvenc (light copies); mjpeg (thumbnails, scrub frames); flac (wav to flac) |
| Decoders | h264, hevc, av1 (export input); aac, flac, pcm (audio) | h264, hevc, av1 with D3D11VA (`-hwaccel d3d11va`); aac; pcm_s16le / pcm_f32le (wav) |
| Muxers | mp4 (mov), flac | mp4, image2 (jpg), flac |
| Demuxers | mov (mp4/m4a), flac | mov, wav, flac |
| Filters | abuffer, abuffersink, aformat, aresample, atempo, volume, scale, format, fps | scale (+ auto-inserted format, null) |
| Parsers / BSF | h264, hevc, av1, aac, flac / extract_extradata, h264/hevc_mp4toannexb, aac_adtstoasc | same |
| Hardware | CUDA + NVENC (ffnvcodec), D3D11VA | D3D11VA, NVENC |
| Libraries | avcodec, avformat, avfilter, avutil, swscale, swresample | same; no avdevice, no postproc |

Not used anywhere: avdevice (the engine linked it only through ffmpeg-next's default "device"
feature, removed on perf/dep-trim), postproc, network protocols, every other codec and format.

## Open choice: clips imported from other recorders

The library imports user folders. A strict build (above) shows no thumbnail for an imported
VP9/WebM, MKV, or Opus/MP3 clip. Adding the common import set (vp9 decode, matroska/webm demux,
opus, mp3) costs roughly 1-2 MB. Recommended.

## Configure line (FFmpeg n7.1.x: the ABI the ffmpeg-next 7.1 bindings expect)

```
./configure --target-os=mingw32 --arch=x86_64 --cross-prefix=x86_64-w64-mingw32- \
  --enable-shared --disable-static --enable-gpl --enable-libx264 \
  --disable-everything --disable-autodetect --disable-doc --disable-debug \
  --disable-avdevice --disable-postproc --disable-network \
  --enable-ffnvcodec --enable-nvenc --enable-cuda --enable-d3d11va \
  --enable-encoder=h264_nvenc,hevc_nvenc,av1_nvenc,libx264,aac,flac,mjpeg \
  --enable-decoder=h264,hevc,av1,aac,flac,pcm_s16le,pcm_f32le,mjpeg \
  --enable-hwaccel=h264_d3d11va,hevc_d3d11va,av1_d3d11va,h264_d3d11va2,hevc_d3d11va2,av1_d3d11va2 \
  --enable-muxer=mp4,mov,ipod,flac,image2 --enable-demuxer=mov,flac,wav,image2 \
  --enable-parser=h264,hevc,av1,aac,flac,mjpeg \
  --enable-bsf=extract_extradata,h264_mp4toannexb,hevc_mp4toannexb,aac_adtstoasc,null \
  --enable-filter=abuffer,abuffersink,buffer,buffersink,aformat,aresample,atempo,volume,scale,format,fps,null,anull \
  --enable-protocol=file,pipe --enable-swscale --enable-swresample
# + for imports: --enable-decoder=vp9,opus,mp3 --enable-demuxer=matroska,mp3 --enable-parser=vp9,opus,mpegaudio
```

Note: `av1` decode in FFmpeg's own decoder needs a hwaccel; on a GPU without AV1 decode, export of an
AV1 clip would fail. Recall records AV1 only on NVENC GPUs that also decode it (RTX 40+), so this holds.

## Build route

1. WSL Ubuntu, `mingw-w64` + `nasm`; nv-codec-headers n12.x (`make install PREFIX=...`); x264 stable
   built static for mingw (`--enable-static --disable-cli`). All in a dated dir under `/home/rohan`
   (ltier rule 2), CPU-pinned the WSL way (no gaming cores: `taskset -c 16-31`).
2. Configure as above, `make -j12`, `make install` into `tools/ffmpeg-dev/ffmpeg-7.1-recall-min`.
3. Point `install.ps1` and the build at it (FFMPEG_DIR); keep the Gyan full build for tests
   (they synthesize clips with libx264 / lavfi `testsrc`, `sine`, `color`: RECALL_FFMPEG in checks).
4. Verify: engine tests, app tests with the full build for fixtures; then engine-cost, export check,
   light-copy hand-over check and a thumbnail pass against the minimal DLLs; `ffmpeg -encoders`,
   `-decoders`, `-filters` listings saved as evidence.

Expected size: about 12-20 MB of DLLs instead of 158 MB (estimate from comparable builds, not measured).
Launch effect: the engine maps fewer and smaller DLLs (less relocation and page-in at start); measure
with `launch-cost.ps1` like the dependency trim.
