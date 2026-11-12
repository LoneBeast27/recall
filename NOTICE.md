# Recall notices

Copyright (C) 2026 LoneBeast27 and Recall contributors.

Original project by **LoneBeast27**. Upstream source and release history live at https://github.com/LoneBeast27/recall.

Recall's own code is licensed under the GNU General Public License, version 3 or any later version. See [LICENSE](LICENSE). When distributing a modified version, preserve applicable copyright and license notices and identify your changes and their dates. Please link back to the upstream Recall repository so users can find the original project.

## Third-party software and research

- [FFmpeg](https://ffmpeg.org/) and its contributors provide encoding, decoding, muxing, and the ffmpeg and ffprobe utilities. Recall uses the [7.1.1 full shared Windows build from Gyan Doshi](https://github.com/GyanD/codexffmpeg/releases/tag/7.1.1). The build uses GPLv3 and identifies its FFmpeg source at [revision db69d06eee](https://github.com/FFmpeg/FFmpeg/commit/db69d06eee). Setup installs the upstream FFmpeg license alongside the runtime.
- [NVIDIA](https://developer.nvidia.com/video-codec-sdk) provides the NVENC hardware encoder used through FFmpeg and [NVAPI](https://github.com/NVIDIA/nvapi) for display controls. The installed NVIDIA driver supplies these APIs under NVIDIA's terms.
- [ledoge and novideo_srgb contributors](https://github.com/ledoge/novideo_srgb) documented the NVIDIA color-space conversion interface used by Recall's SDR gamut clamp. See the [novideo_srgb GPLv3 license](https://github.com/ledoge/novideo_srgb/blob/master/LICENSE).
- [Intel PresentMon and its contributors](https://github.com/GameTechDev/PresentMon) provide the ETW event reference used by Recall's frame-timing implementation. See the [PresentMon MIT license](https://github.com/GameTechDev/PresentMon/blob/main/LICENSE.txt).
- [AMD FidelityFX Super Resolution 1](https://github.com/GPUOpen-Effects/FidelityFX-FSR) (EASU upscale and RCAS sharpen) is compiled into Recall's upscaled export from AMD's `ffx_a.h` and `ffx_fsr1.h`, copyright Advanced Micro Devices, Inc. Its MIT license is reproduced in full under "AMD FidelityFX FSR 1 license" below (also in [LICENSE-FidelityFX-FSR.txt](recall/src/export/upscale/fsr1/LICENSE-FidelityFX-FSR.txt)).
- NVIDIA RTX Video Super Resolution is part of the installed NVIDIA driver. Recall asks the driver for it and ships none of its files.
- [Microsoft](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist) provides the Visual C++ x64 Redistributable, which Setup installs when Windows needs it. It remains under Microsoft's terms.

## Bundled assets

- Geist and Bricolage Grotesque use the SIL Open Font License 1.1. The full notices are in [FONTS-LICENSE.txt](recall-native/assets/fonts/FONTS-LICENSE.txt).
- Lucide and Feather icon notices are in [LICENSE-lucide.txt](recall-native/assets/icons/LICENSE-lucide.txt). Other app icon credits are in [brand-attribution.txt](recall-native/assets/icons/brand-attribution.txt).
- The video preview source and its CC0 credit are described in [the quality preview README](recall-native/assets/quality/README.md).

## AMD FidelityFX FSR 1 license

```
Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

Each release identifies the corresponding Recall source revision. Release Setup obtains FFmpeg and Visual C++ separately from their providers.
