# Recall

A local Windows clip recorder, an HDR display-manager companion, and a Svelte/Tauri control window.

## Recording and export

Video encoding requires NVIDIA NVENC (`h264_nvenc`); there is no CPU video fallback. Initialization or runtime failure is reported as a recording error.

Recordings retain native Windows Graphics Capture timestamps in a VFR timeline. Save keeps the recorded timing. Export creates a separate CFR file: 60 fps by default, with 120/240/480/960 fps presets and a custom positive integer rate. The source remains intact. Missing output frames repeat the latest captured frame; motion interpolation is not implemented. Export frame rate does not guarantee the same capture throughput.

In Recall Hub, select the CFR export rate above Recent and use a clip's Export button. Exports go into an `exports` directory beside the source; the completion message can reveal the result in Explorer.

The capture path still uses the existing CPU readback and GPU processing. Actual capture throughput depends on the display, capture delivery, and machine load. `RECALL_FPS` is a nominal NVENC/GOP hint; it no longer drives a CFR recording pacer. `RECALL_BUFFER_SECS` sets replay retention (default 60 seconds). Audio currently records the Game and Mic endpoints.

## Clip editor

Open **Clips**, choose **Edit**, then set in/out points, split retained segments, or delete a segment. Undo/redo restores edits. Each audio stream has an audible preview gain and mute control. **Save edits** writes a versioned source-adjacent `.recall-edit.json` sidecar; the recording stays unchanged.

The editor prepares source-aligned 48 kHz WAV stems and real waveform peaks in its application cache. Source size, modification time and cache format version invalidate stale previews. Project mismatches load a fresh plan with a visible warning.

**Export** creates a separate NVENC CFR file under the recording's `exports` directory and works while the recorder/controller is offline. Cuts close gaps; audio follows the same retained ranges, with separate AAC tracks and applied levels/mutes. CFR rates default to 60 fps with 120/240/480/960/custom options. Edited audio uses a fallible 256 MiB PCM budget per track and reports an error above that limit. This is a single-source editor.

Shortcuts outside inputs: Space play/pause, I/O set in/out, C or S split, Delete remove the selected segment, Ctrl+Z/Y undo/redo. Capture still records the Game and Mic endpoints; the Audio page's additional meters do not create extra recorded tracks.

## Status overlay

The sidebar **Overlay** switch starts the separate native `recall-overlay.exe`; choose any screen corner. It starts off until enabled and remembers that setting. It runs with the Hub and exits when its owning Hub closes.

The overlay stays hidden during normal recording. It briefly appears after a clip is saved (3 seconds), microphone mute changes (2 seconds), or a save/encoder failure (4 seconds), then hides automatically. Regular status updates do not keep it visible. While shown, it includes actual replay readiness, buffered seconds, capture frame rate and NVENC state. It never reports capture FPS as game FPS. The native Win32/GDI window remains click-through without taking focus and requests Windows capture exclusion. Exclusive-fullscreen and anti-cheat behavior still need a real-game compatibility check.

The bounded idle measurement used 2.38 MiB private memory and 9.11 MiB working set, with no measurable CPU time over 8 seconds. This is a small local sample, not a long-session performance guarantee. Native styles and capture-exclusion acceptance are recorded under `.planning/evidence/overlay`.

## Valorant test session

Run the current session launcher from PowerShell:

```powershell
powershell -NoProfile -File L:/General/recall/Start-Recall.ps1 -OpenUI -Overlay
```

Valorant must already be running. This machine's verified CCD1 is logical CPUs 16-31. The launcher pins the controller, recorder, native app and overlay to that group, captures the Valorant window, and records game audio through Windows per-process loopback. It keeps microphone audio separate. It does not change display settings or Windows audio routing.

- **Alt+N:** save the last 120 seconds; the available keyframe window can be slightly shorter.
- **Alt+M:** toggle microphone audio in future recorded samples. Game audio continues.
- Stop the recording controller with `powershell -NoProfile -File L:/General/recall/Start-Recall.ps1 -Stop`.
- Clips go to `V:\ALL RECORDINGS`. The current live test produced a verified 119.146-second clip with both audio tracks.
- The live capture samples ranged from 45 to 60 fps. A long save took about 7 seconds and blocks the encoder; this remains a known issue.
- The native GPUI client is a migration build. Its full UI/media benchmark is pending; the working Tauri implementation remains available.

## Run the current build

- Recorder/controller: `target-dev\debug\recall-hdr.exe` (supervises the sibling `recall.exe`).
- Hub: `recall-hub\src-tauri\target\release\recall-hub.exe`.
- Native overlay: `target-dev\debug\recall-overlay.exe` (normally launched by the Hub).
- Headless export: `target-dev\debug\recall.exe --json --export-cfr <input> <different-output.mp4> [fps]`; the output directory must already exist.
- Headless edited export: `target-dev\debug\recall.exe --export-edit <input> <different-output.mp4> <fps> <project.json> --json`.
- Bounded synthetic encoder diagnostic: `target-dev\debug\recall.exe --encoder-smoke <output-directory>`; initializes NVENC and follows the shared GPU resource policy.

The recorder needs the bundled FFmpeg runtime DLLs beside its executable, or the FFmpeg `bin` directory on PATH.

## Build

Use the shared desktop's `C:/Users/rohan/.claude/tools/nice-build.ps1` wrapper for builds. From this repository, set:

```powershell
$env:FFMPEG_DIR = Join-Path (Get-Location) 'tools\ffmpeg-dev\ffmpeg-n7.1-latest-win64-gpl-shared-7.1'
$env:LIBCLANG_PATH = Join-Path (Get-Location) '.tmp\nvenc-vfr\clang18\clang\native'
$env:CARGO_TARGET_DIR = Join-Path (Get-Location) 'target-dev'
powershell -NoProfile -File C:/Users/rohan/.claude/tools/nice-build.ps1 cargo build -p recall -p recall-overlay --offline
```

Clang 18 (`libclang==18.1.1`) was installed in that local tooling directory for this build. System LLVM 22 generated incompatible FFmpeg layouts. If the local tooling is removed, restore the pinned package into `.tmp/nvenc-vfr/clang18` before rebuilding. The current Rust 1.94 toolchain emits a warning against the machine's 1.95 pin; the wrapper treats that as a warning.

Hub build: from `recall-hub`, run the same wrapper with `C:/Program Files/nodejs/npm.cmd run tauri -- build --no-bundle`. The equivalent `run check` performs Svelte/TypeScript validation.

## Project records

- [Current implementation contract](.planning/SPEC.md)
- [Current status and verification](.planning/STATE.md)\n- [Single-clip editor contract](.planning/EDITOR_SPEC.md)\n- [Interface and native overlay design](.planning/DESIGN_SPEC.md)
- Original recording design: `L:\Docker Speed Drive\Agents\Claude\PRDs\recall-hdr-clip-tool-PRD.md`.

The 17 October 2026 VFR recording decision supersedes the older design's CFR-only recording requirement. CFR remains the export format.

## Workspace layout

- `recall/`: capture, NVENC replay recording, audio, Moments insertion, and CFR export.
- `recall-hdr/`: display-manager tray/controller and engine supervisor.
- `recall-hub/`: Svelte/Tauri library, editor and controls.
- `recall-overlay/`: native status HUD and pipe reducer.
- `tools/`: test tools and local media dependencies.
