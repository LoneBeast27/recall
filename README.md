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
- Close Recall first: while the app runs, the installer stops with exit 3 and changes nothing.
  `-CloseApp` closes the app for you instead.
- Restarting the recorder (an install, or `{"cmd":"quit"}` on its pipe) leaves the app open; it
  reconnects to the next recorder by itself. Only **Quit Recall** in the tray closes the app.

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
- **Recall Island** (the in-game overlay): a small pill that drops in when a game is picked up
  ("Recording VALORANT"), when recording switches to the desktop, pauses or stops, and for saves,
  bookmarks and screenshots. **Alt + ` then 5** (or the gauge button in the app's title bar, or
  Settings → General) toggles live performance stats: FPS with its 1% low, GPU and CPU rings,
  and the recording dot. Each game gets its own spot, measured from your clips so the Island
  stays clear of that game's HUD (`hud-spots.json`); unknown games use the top right.
- **Tray menu:** status · Open Recall · Stop/Start recording · Quit Recall.
- **The app** (Start menu → Recall): library grouped by day, hover a tile to scrub, favourite,
  rename, delete to the Recycle Bin, drag a clip into Discord or copy it. Right-click a tile for
  Open, Favourite, Rename, Copy, Show in folder and Delete (on the whole selection when the tile
  is selected; Delete asks first and takes thumbnails, edits and bookmarks along). Renaming saves on
  Enter, Tab or a click anywhere else; Esc keeps the old title. Click a tile to edit:
  trim, split, cut, per-track volume, slow motion or speed-up per section, then export
  (re-encode or instant lossless trim, frame rate, resolution, size limit such as 10 MB for
  Discord). Edits live in a small file beside the clip and apply only on export, so the original
  never changes; undoing every edit removes that file and the Edited label, unless the clip has
  an export. Cancel export stops
  a running export and leaves nothing behind.
- **Old clips:** Settings → Media → Add folder lists clips from ShadowPlay, Medal, OBS or any
  folder in the library. Nothing is moved.
- **Clip folders stay in step:** while the app is open, a clip another program copies, renames or
  deletes in a clip folder shows up within seconds, once the file is finished. A renamed clip
  loses its favourite and title, which are stored by file name. An imported folder on a USB drive
  may need the app closed before Windows lets you safely remove the drive.

### FPS numbers (Windows frame timing)

FPS and 1% low come from Windows' own present events (ETW), the method Intel PresentMon and
NVIDIA FrameView use: nothing is injected into games and no extra tool runs. While stats are on,
the overlay keeps a small trace session ("RecallFrameTiming") on the game's process; it stops
when stats turn off. Reading these events needs the "Performance Log Users" group: the installer
adds you through one administrator prompt (`-NoFrameTiming` skips it), and it takes effect at
your next sign-in. Until then the Island shows "No frame timing".

RTSS is not used. (An earlier build ran a private RTSS copy; exiting RTSS while a game had its
hook loaded crashed VALORANT, so it was removed on 2026-10-26.)

## Settings

| Tab | What it holds |
|---|---|
| General | Start with Windows, Island notices, performance stats, quit the recorder |
| Clip | automatic recording, desktop between games, replay length, format (H.264 / HEVC / AV1), frame rate, bitrate, encoder effort, voice chat on its own track |
| Media | clip folders, storage limit (oldest clips go to the Recycle Bin; favourites and edited clips stay), SSD cache size |
| Shortcuts | the in-game shortcuts and the two-step switch |

The Audio page shows live meters for every source.

HDR switching (turning the monitor's HDR on for HDR games, the manual HDR toggle and its
hotkey, the desktop colour profile, the NvAPI clamp test and the Display & HDR page) is
experimental and shelved since 2026-11-04: the recorder never changes a display setting
and the page is hidden, even if `behavior.hdr_auto = true` is in the config. The code is
kept; start the recorder and the app with `RECALL_EXPERIMENTAL_HDR=1` to try it (see
`recall-hdr/src/hdr_gate.rs`). Recording itself is unchanged: an HDR desktop is still tone
mapped into SDR clips.

On an SDR display Recall captures 8-bit colour (half the GPU memory traffic); on HDR it
captures half-float. When the display turns from SDR to HDR while a game records (an HDR
switch, or Windows HDR turned on), capture restarts in half-float and the replay buffer
starts again from that moment: a save right after the switch holds only the time since.
Switching back from HDR to SDR keeps the running capture and the buffer.

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
| Player log (stalls, audio underruns) | `%APPDATA%\Recall\player.log` |

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

After any change to the clip folders' handling, run `scripts\dev\watch-check.ps1` (private
instance, sandbox folders). `RECALL_NATIVE_LAYOUT=<file>` makes the app keep a JSON dump of tile
and button bounds for external testers; it is a dev format, off unless set.

After any change to playback, editing or layout, run the viewer check against the release
build. Each scenario starts a private Recall instance outside every monitor (it never takes focus,
and has its own settings, library, cache and control pipe), drives it with the inputs a person
uses -- Space, J/K/L, arrows, the play button, the timeline, trim handles, tools, speed, the
Library -- and checks from the app's trace that the clock *and the picture* move, start where
they should and stop where they should:

```powershell
powershell -NoProfile -File scripts\dev\viewer-check.ps1 [-ShortClip <~10 s clip>] [-Only '*end*']
```

The player rules it holds the app to: play inside a kept part plays from the playhead; play in a
cut or at a segment's end plays from the next segment; play at the end of the last segment (or
after the clip finished) starts over from the first kept frame. The picture is never sent onto
its last frame: a seek at or past where that frame starts ends the Media Foundation session, the
position falls to 0 and the video can freeze for good (2026-10-30, "trimmed and player stopped
working", "completing the clip watch doesnt play again"). A stream-copied cut can hold its last
frame for 0.5 s, so the playhead may rest past the picture; the trace's `picture_end` says where
the picture stops. The player opens only the file's picture stream: the sound comes from the
stems, and a selected sound stream that ends early makes seeks past it fail.

The Moments scenario plays a SteelSeries Moments clip and checks its sound. Moments stamps its audio frames
1000 or 1100 ticks apart instead of 1024; the editor used to follow those stamps and played
choppy audio (fixed 2026-10-27: samples stay back to back unless the stamps drift by more than
40 ms). `RECALL_AUDIO_SOAK=<folder of stems> cargo test audio_soak -- --ignored --nocapture`
plays stems silently on the default device and reports output underruns.

Checks stay silent and leave the real mic alone, with no driver to install and no change to the
Windows default devices. A test instance (`RECALL_NATIVE_OFFSCREEN=1`) and every unit test write
zeros to the default output, so its clock and underruns stay real and nothing is heard. A test
instance opens no level meters. The engine cost check records its Mic track from an endpoint
nobody speaks into, and `RECALL_AUDIO_EXACT=1` stops it falling back to the real mic; without that
endpoint it warns and measures with no Mic track.
`cargo run --release --example audio_poll -- --pid <pid> --stop <file>` proves it: it reads what
that process tree sends to Windows, and prints the peak.

After any change to capture or encoding, run the engine cost check (never while a game runs; the
display must be on, since Windows sends no capture frames while it sleeps). It records a
2560x1440 window presenting at ~480 fps the way a game is recorded and fails when the engine's
CPU per captured frame regresses; `-SaveSeconds 10` also saves a clip for
`python -I scripts\dev\check-clip.py <clip>` (frame counter continuity, colour swatches):

```powershell
cargo build --release -p recall --bins --example present_target --offline   # via nice-build.ps1
powershell -NoProfile -File scripts\dev\engine-cost.ps1 [-Seconds 30] [-MaxMsPerFrame 1.5]
```

The app uses its own target folder (`target-gpui`) because GPUI pins different dependency
versions. Tests that need NVENC or a live game are `#[ignore]`d. Status, verification and
open items: [.planning/STATE.md](.planning/STATE.md); the scope and every ruling:
[.planning/FINISH_SPEC.md](.planning/FINISH_SPEC.md).
