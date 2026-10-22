# Recall

A Windows game-clip recorder. It keeps the last minutes of any game in memory, saves a clip
when you press a key, and gives you a library and editor to trim, cut and share clips.

Recall needs an NVIDIA GPU (NVENC). There is no CPU video encoder.

## Install

Run from PowerShell in this folder:

```powershell
powershell -NoProfile -File scripts\install.ps1
```

- Installs to `%LOCALAPPDATA%\Programs\Recall` (no admin rights). `-InstallDir <folder>` picks
  another folder; it must be on an SSD, because the app keeps its preview cache there.
- Adds **Recall** to the Start menu, starts the recorder, and sets it to start with Windows
  (`-NoAutostart` skips that; Settings → General turns it off later).
- Builds run on the background CPU cores (CCD1) through `nice-build.ps1`, so a running game
  is not slowed down.

## Use it

- **Play a game.** Recording starts by itself when a fullscreen game is in front and stops
  when it closes. The tray icon is red while recording and grey while paused.
- **Alt+N** saves the last 2 minutes (Settings → Clip → Replay length). A soft tick confirms the
  press at once and a chime plays when the clip is safe; the small in-game notice shows it too.
  Clips are written to the SSD first (about a second) and moved to the clip folder in the
  background, so the hard drive's speed never delays a save. **Alt+M** mutes your microphone
  in clips.
- **Two-step shortcuts:** press **Alt + `** (the key left of 1), then
  **1** bookmark · **2** save the last 30 s · **3** screenshot · **4** start/stop a full
  recording. The game never receives the number. Turn them off in Settings → Shortcuts.
- **Tray menu:** status · Open Recall · Stop/Start recording · Quit Recall.
- **The app** (Start menu → Recall): library grouped by day, hover a tile to scrub, favourite,
  rename, delete to the Recycle Bin, drag a clip into Discord or copy it. Double-click to edit:
  trim, split, cut, per-track volume, slow motion or speed-up per section, then export
  (re-encode or instant lossless trim, frame rate, resolution, size limit such as 10 MB for
  Discord).
- **Old clips:** Settings → Media → Add folder lists clips from ShadowPlay, Medal, OBS or any
  folder in the library. Nothing is moved.

## Settings

| Tab | What it holds |
|---|---|
| General | Start with Windows, save notice overlay and its corner, quit the recorder |
| Clip | automatic recording, desktop between games, replay length, format (H.264 / HEVC / AV1), frame rate, bitrate, encoder effort, voice chat on its own track |
| Media | clip folders, storage limit (oldest clips go to the Recycle Bin; favourites and edited clips stay), SSD cache size |
| Shortcuts | the in-game shortcuts and the two-step switch |

The HDR page switches the monitor between HDR and SDR per game; the Audio page shows live
meters for every source.

## Where things live

| What | Where |
|---|---|
| Clips | `V:\ALL RECORDINGS` by default (`recording.output_dir`) |
| Screenshots | `<clips>\screenshots` |
| Recorder settings | `%APPDATA%\recall-hdr\config.toml` |
| Favourites, titles, clip folders | `%APPDATA%\Recall\library.json`, `folders.json` |
| Edits | `<clip>.recall-edit.json` beside each clip |
| Preview cache | `<install folder>\cache`, one budget (2 GB by default) |
| Clips on their way to the clip folder | `<install folder>\cache\staging` (moved within seconds; recovered on next start) |
| Player stall log | `%APPDATA%\Recall\player.log` |

Clips on a hard drive are read on one background lane per disk at low I/O priority (SMR
drives are detected); thumbnails, hover frames and recently watched clips are kept on the SSD.

## Parts

| Folder | Program | Job |
|---|---|---|
| `recall/` | `recall.exe` | capture engine: Windows Graphics Capture → NVENC → memory ring; saves, exports, long recordings |
| `recall-hdr/` | `recall-hdr.exe --record` | controller + tray: finds the game, runs the engine, hotkeys, HDR switching, config, control pipe `\\.\pipe\recall-hdr-control` |
| `recall-overlay/` | `recall-overlay.exe` | the small in-game save notice and two-step shortcut hint |
| `recall-native/` | `recall-native.exe` | the app (GPUI): library, editor, HDR, audio, settings |
| `recall-hub/` | — | the old Svelte/Tauri window; replaced by `recall-native`, kept for reference |

## Build and test (developers)

```powershell
$env:FFMPEG_DIR    = "$PWD\tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared"   # has av1_nvenc
$env:LIBCLANG_PATH = "$PWD\.tmp\nvenc-vfr\clang18\clang\native"              # clang 18 for ffmpeg-sys
$env:CARGO_TARGET_DIR = "$PWD\target-dev"
powershell -NoProfile -File C:/Users/rohan/.claude/tools/nice-build.ps1 cargo test -p recall -p recall-hdr -p recall-overlay --offline
$env:CARGO_TARGET_DIR = "$PWD\target-gpui"
powershell -NoProfile -File C:/Users/rohan/.claude/tools/nice-build.ps1 cargo test --manifest-path recall-native\Cargo.toml --offline
```

After any change to playback, editing or layout, run the viewer check against the release
build. It drives the real app with key and mouse input and times the player's responses:

```powershell
powershell -NoProfile -File scripts\dev\viewer-check.ps1
```

The app uses its own target folder (`target-gpui`) because GPUI pins different dependency
versions. Tests that need NVENC or a live game are `#[ignore]`d. Status, verification and
open items: [.planning/STATE.md](.planning/STATE.md); the scope and every ruling:
[.planning/FINISH_SPEC.md](.planning/FINISH_SPEC.md).
