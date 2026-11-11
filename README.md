# Recall

A Windows game-clip recorder. It keeps the last minutes of any game in memory, saves a clip
when you press a key, and gives you a library and editor to trim, cut and share clips.

Recall needs an NVIDIA GPU (NVENC). There is no CPU video encoder.

## Source and license

Recall was created by **LoneBeast27**. Official Recall downloads are free. Recall’s own code is released under [GPL-3.0-or-later](LICENSE), which allows commercial use and redistribution. If you distribute a modified build, keep the copyright and license notices and clearly mark your changes and their dates. The [upstream Recall repository](https://github.com/LoneBeast27/recall) is the reference for the original source. See [NOTICE.md](NOTICE.md) for credits and third-party terms.

## Privacy

Clips, screenshots, edits, the library and settings stay on your PC; Recall has no account,
telemetry or analytics. The only network traffic is:

- **Update check:** official versioned builds ask the GitHub API for the latest release
  (`api.github.com/repos/LoneBeast27/recall/releases/latest`, then a compare of its tag with
  `main`) shortly after the app opens. Turn it off with **Settings → General → Check for updates
  automatically**; nothing is then sent unless you press **Check now**. An update's
  `RecallSetup.exe` is downloaded from the GitHub release only when you choose to install it.
- **Setup opened by hand** (not by the app's update): over an existing install, it asks the GitHub
  API for the latest release (the same two requests as the app's update check) to tell you whether
  a newer version exists. It skips this when **Check for updates automatically** is off. Nothing
  about you is sent.
- **Setup:** downloads the pinned FFmpeg runtime from Gyan's GitHub release and, when it is
  missing, the Microsoft Visual C++ x64 runtime from Microsoft; on Windows N it can add the
  Media Feature Pack through Windows Update.
- **Installing an update** keeps a backup of your settings and a small resume note in
  `%LOCALAPPDATA%\Recall\updates\` on your PC; nothing about them is sent.

Nothing else is sent. Details: [Privacy](CODE_SIGNING_POLICY.md#privacy).

## Install

Recall releases are not code-signed yet, so Windows SmartScreen may say "Windows protected your PC": choose **More info**, then **Run anyway**. Download only from [Releases](https://github.com/LoneBeast27/recall/releases) and check the installer against `SHA256SUMS.txt` with `Get-FileHash RecallSetup.exe`. Details in the [code signing policy](CODE_SIGNING_POLICY.md).

Open `dist/recall-current/RecallSetup.exe` for Quick Setup. Choose whether recording starts when setup is complete and whether to set up Windows game detection and FPS. **Change…** opens the SSD install folder picker from Quick Setup. **Advanced** also lets you type a folder or change the Start menu shortcut. After installation, **Continue setup** opens Storage, Audio, and Recording quality. Recording quality compares the same full frame before and after encoding, with a detail crop available. Choose **Open Recall** to reach your library. Updates keep your existing clip folder and recording settings.

Setup checks the PC before copying Recall. Release Setup downloads the pinned FFmpeg runtime from Gyan and the signed Microsoft Visual C++ x64 runtime when required. On Windows N, it adds the Media Feature Pack through Windows Update when needed. Windows may ask for administrator permission. If a restart is needed, reopen Setup after restarting. Game recording uses the NVIDIA driver installed for your GPU.

### Updates

Official versioned builds check [Recall's GitHub Releases](https://github.com/LoneBeast27/recall/releases) after launch. You can also check in **Settings → General → About**. Recall offers a newer stable release once it verifies that the tag is on `main`. It verifies the Setup download against GitHub's SHA-256 before opening Setup for confirmation.

Feature releases are built by the [release workflow](.github/workflows/release.yml) on a GitHub-hosted runner, as described in the [code signing policy](CODE_SIGNING_POLICY.md) (releases are currently unsigned). `scripts\release.ps1 -Version <major.minor.patch> -ArtifactDirectory <extracted CI bundle>` checks that bundle (release tag on `main`, source revision, SHA-256, version) and refuses an unsigned Setup once signing is configured. Urgent hotfixes, for bugs that affect users right now, may be built locally from the tagged public source: run `powershell -NoProfile -File scripts\release.ps1 -Version <major.minor.patch> -FfmpegArchive <zip> -FfmpegDirectory <bin folder> -VcRedist <vc_redist.x64.exe>` from a clean checkout at the `v<major.minor.patch>` tag on `main` (`Get-Help scripts\release.ps1 -Full` lists every step). It runs the tests, strips local paths from the build, refuses any build that still contains them, and scans the result with Defender. Pushing the tag still runs the workflow as an independent re-check. Recall 0.1.1 was built locally from its tagged source with `scripts\release.ps1`; its `SOURCE-REVISION.txt` names that commit. Every release's `SOURCE-REVISION.txt` names the commit it was built from. The output contains `RecallSetup.exe`, `SHA256SUMS.txt`, `SOURCE-REVISION.txt`, and the Recall executables in `recall-app` for individual scans. A draft can be verified before publishing.

Run `Get-Help scripts/dev/scan-release.ps1 -Examples` for a per-file Defender scan of the installer, app binaries, and vendor downloads.

Fresh profiles use the Windows default microphone and system output. A distinct Windows communications output gets its own track. Add extra devices or apps from **Input**, **Output**, and **Application audio**. Select the icon on a custom strip to change it.

### RecallTesting

Run `dist/recall-testing/RecallTesting.exe` to rehearse setup with demo drives, audio sources, and a demo library. Changes stay in memory for that session. The testing executable has no connection to the recorder and uses no Recall profile or clip files.

To build and install Recall from source, run this in the repo:

```powershell
powershell -NoProfile -File scripts\install.ps1
```

- Installs to `%LOCALAPPDATA%\Programs\Recall` (no admin rights). `-InstallDir <folder>` picks
  another folder; it must be on an SSD, because the app keeps its preview cache there.
- Adds **Recall** to the Start menu and starts the recorder. Start with Windows is off unless
  you ask: `-Autostart` turns it on, or Settings → General. An update leaves the setting as it is.
- The folder must be on a drive Windows reports as an SSD. For an SSD it can't classify (USB,
  RAID, Storage Spaces, VM disk), `-AllowUnknownDrive` installs with a warning.
- Builds run on the background CPU cores (CCD1) through `nice-build.ps1`, so a running game
  is not slowed down.
- Close Recall first: while the app runs, the installer stops with exit 3 and changes nothing.
  `-CloseApp` closes the app for you instead.
- Restarting the recorder (an install, or `{"cmd":"quit"}` on its pipe) leaves the app open; it
  reconnects to the next recorder by itself. Only **Quit Recall** in the tray closes the app.

## Use it

- **Play a game.** Recording starts by itself when a game is in front and stops when it
  closes. The tray icon is red while recording and grey while paused.
- **What counts as a game.** Recall records an app on its own only with official proof that it
  is a game:
  - it is installed in a Steam library (`steamapps\common`), or in an Epic, GOG, Ubisoft
    Connect, EA app, Battle.net or itch install folder (read from the launchers themselves, one
    folder per installed game; a GOG entry whose folder is a broad parent such as `D:\Games`
    counts only for the exe that entry names), windowed too;
  - it is an Xbox / Microsoft Store game (its package holds `MicrosoftGame.config`, in an
    `XboxGames` folder or `WindowsApps`), while fullscreen;
  - Recall knows it (its built-in game list) or the NVIDIA driver lists it as a game;
  - or, fullscreen, it draws like a game (exclusive fullscreen, its own flip, or tearing
    presents). This check needs trace rights (the installer adds you to Performance Log Users).
    Without them, or when the check cannot start, an app with none of the proof above is not
    recorded.
  Anything else is not recorded. After 20 s in front the notice says "Not recording <exe>: add it
  as a game in Settings" (once per exe per run), and the tray menu gets "Add <exe> as a game".
- **Settings → Games** lists the apps you registered. **Add app…** picks a running app (the one
  that was in front comes first, borderless games included) or browses to an exe; it is then
  recorded in any window. Your own entry beats every list below: a browser, a media player, a
  launcher, a remote-desktop or game-streaming client (Parsec, Moonlight) or a call or chat app
  you add is recorded. Recall itself and parts of Windows (the desktop and taskbar, the lock and
  sign-in screens, screensavers, system processes) are never recorded and cannot be added. Each
  entry can be set to **Never record** (never recorded, even from a Steam library) or removed.
  Never record or Remove on the app being recorded stops recording it at once and throws away
  its unsaved replay buffer; a clip already being saved still finishes (saved clips are never
  touched).
  In config.toml these are `[[overrides]]` entries (`exe = "Game.exe"` or `"game"`, `kind =
  "sdr_game"` / `"hdr_game"` / `"none"`).
- **Call, chat, mail, office, media-player and music apps** (Zoom, Teams, WhatsApp, Kodi,
  Stremio, VLC, LibreOffice, Spotify and others) are never recorded on their own, even when they
  sit in a launcher folder or a Store game package or draw like a game; only a Steam library, a
  game Recall knows, or your own entry beats that. Discord is the exception: its fullscreen
  window, a fullscreen call or stream view included, is recorded by default (config.toml
  `[detect] fullscreen_games = ["Discord"]`, shown in Settings → Games as "Recorded when
  fullscreen"; remove it there to stop). Windowed Discord is never recorded: pressing Esc
  (or restoring the window) ends its recording at once (a clip being saved still finishes); alt-tab away from a still-fullscreen
  Discord keeps it, like a game. Adding an exe to
  `fullscreen_games` records it only while it is fullscreen; a Settings → Games entry records
  every window of that exe, windowed ones too (a meeting toolbar or a chat window). When a
  blocked fullscreen app draws like a game (its own flip or exclusive fullscreen, not a composed
  video call) for over 20 s, the same notice names it (the log names every blocked exe).
- **Alt+N** saves the last 2 minutes (Settings → Recording → Replay length). A soft tick confirms the
  press at once and a chime plays when the clip is safe; the small in-game notice shows it too.
  Clips are written to the SSD first (about a second) and moved to the clip folder in the
  background, so the hard drive's speed never delays a save. **Alt+M** mutes your microphone
  in clips. A line-in or capture card can skip it (Audio page → the strip's menu); Recall knows
  that device by its Windows endpoint, which for two identical USB devices without a serial
  number follows the USB port, so keep each of them in the same port.
  Two devices with the same name are told apart by that endpoint: a source records the one it
  was picked on, and waits (silent) when neither is it.
- **Two-step shortcuts:** press **Alt + `** (the key left of 1), then
  **1** bookmark · **2** save the last 30 s · **3** screenshot · **4** start/stop a full
  recording. The game never receives the number. Turn them off in Settings → Shortcuts.
- **Recall Island** (the in-game overlay): a small pill that drops in when a game is picked up
  ("Recording VALORANT"), when recording switches to the desktop, pauses or stops, and for saves,
  bookmarks and screenshots. **Alt + ` then 5** (or the gauge button in the app's title bar, or
  Settings → General) toggles live performance stats: FPS with its 1% low, GPU and CPU rings,
  and the recording dot. Each game gets its own spot, measured from your clips so the Island
  stays clear of that game's HUD (`hud-spots.json`); unknown games use the top right.
  The pill uses compact spacing and stacks text upright along a side edge.
  The overlay shortcut (Alt+O by default) hides or shows notices and live stats together.
- **Tray menu:** status · Open Recall · (after a "Not recording" notice: Add <exe> as a game) ·
  Stop/Start recording · Quit Recall.
- **The app** (Start menu → Recall): library grouped by day, hover a tile to scrub, favourite,
  rename, delete to the Recycle Bin, drag a clip into Discord or copy it. Right-click a tile for
  Open, Favourite, Rename, Copy, Show in folder and Delete (on the whole selection when the tile
  is selected; Delete asks first and takes thumbnails, edits and bookmarks along). Renaming saves on
  Enter, Tab or a click anywhere else; Esc keeps the old title. Click a tile's picture to edit
  it (a click on its title opens it after the double-click time; a double-click on the title
  renames). Shift+click selects the run from the last clicked tile and Ctrl+click adds or removes
  one, without opening anything. In the editor:
  trim, split, cut, per-track volume, slow motion or speed-up per section, then export
  (re-encode or instant lossless trim, frame rate, resolution, size limit such as 10 MB for
  Discord). Edits live in a small file beside the clip and apply only on export, so the original
  never changes; undoing every edit removes that file and the Edited label, unless the clip has
  an export. Cancel export stops
  a running export and leaves nothing behind.
- For a re-encoded export with a size limit, choose **Motion** to keep the selected FPS or **Detail** to trade frame rate for resolution. Both modes choose the resolution automatically. Switch to **Manual** to edit frame rate and resolution. The finished export shows its actual size, resolution, and FPS.
- **Folder Mapper:** In Settings → Storage or the Storage step in setup, select **Link folder** and choose a recording folder from ShadowPlay, Medal, or OBS. Recall watches for new clips and links them in place. Edits go into Recall’s library data and exports go to Recall’s export folder. Removing a linked clip only hides its entry in Recall. Former Recall recording folders remain listed under **Folders Recall manages**.
- **Clip folders stay in step:** while the app is open, a clip another program copies, renames or
  deletes in a clip folder shows up within seconds, once the file is finished. A renamed or moved
  clip loses its favourite and title, which are stored by the clip's full path (two clips with the
  same name in different folders keep their own). An imported folder on a USB drive
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
| Media | clip folders, storage limit (oldest clips go to the Recycle Bin; favourites, edited clips, clips played in the last 10 minutes and the clip open in the app stay), SSD cache size |
| Shortcuts | the in-game shortcuts and the two-step switch |

The Audio page shows live meters for every source.

> **Warning:** the experimental HDR features below change Windows display settings for the
> whole system: the monitor's HDR mode, the SDR content brightness, the desktop colour profile,
> and the NVIDIA (NvAPI) colour gamut clamp. Recall puts them back when it exits, and the next
> recorder start repairs what a crash left switched; until then they stay changed for every
> app (reset them in Windows Settings → Display or the NVIDIA Control Panel, or run
> `recall-hdr.exe --restore`). They are off unless you opt in.

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
| Clips | `<Videos>\Recall` by default (Settings → Storage) |
| Screenshots | `<clips>\screenshots` |
| Recorder settings | `%APPDATA%\recall-hdr\config.toml` |
| Favourites, titles, clip folders | `%APPDATA%\Recall\library.json`, `folders.json` |
| Edits | `<clip>.recall-edit.json` beside each clip |
| Preview cache | `<install folder>\cache`, one budget (2 GB by default on the system drive; set in Settings) |
| Clips on their way to the clip folder | `<install folder>\cache\staging` (moved within seconds; recovered on next start) |
| Player log (stalls, audio underruns) | `%APPDATA%\Recall\player.log` |

Clips on a hard drive are read on one background lane per disk at low I/O priority (SMR
drives are detected); thumbnails, hover frames and recently watched clips are kept on the SSD.

## Parts

| Folder | Program | Job |
|---|---|---|
| `recall/` | `recall.exe` | capture engine: Windows Graphics Capture → NVENC → memory ring; saves, exports, long recordings |
| `recall-hdr/` | `recall-hdr.exe --record` | controller + tray: finds the game, runs the engine, hotkeys, HDR switching, config, control pipe `\\.\pipe\recall-hdr-control-<session id>` (this user and SYSTEM only) |
| `recall-overlay/` | `recall-overlay.exe` | the small in-game save notice and two-step shortcut hint |
| `recall-native/` | `recall-native.exe` | the app (GPUI): library, editor, HDR, audio, settings |
| `recall-hub/` | — | the old Svelte/Tauri window; replaced by `recall-native`, kept for reference |

## Build and test (developers)

To sign a separate copy of a built installer, run `scripts/setup/sign.ps1` with `-InputPath`, a new `-OutputPath`, the `-CertificateThumbprint` of your trusted code-signing certificate, and its HTTPS `-TimestampUrl`. The script verifies the publisher and timestamp before writing the signed output.

Build the Windows setup screen with `powershell -NoProfile -File scripts/setup/build.ps1 -Preview`. Open `dist/setup/RecallSetupPreview.exe` to rehearse setup. The release setup build takes a verified payload via `-PayloadDirectory`. Setup installs, updates and uninstalls in its own C# (`scripts/setup/Setup*.cs`) and never starts PowerShell; `scripts/install.ps1` and its helpers remain developer tools. Setup registers Recall in Apps & Features; `RecallSetup.exe /uninstall` removes only the files its `install-manifest.json` lists and keeps the cache unless asked. Run the Setup unit tests with `scripts/setup/build.ps1 -Test`. Pass `-VCRedistPath` to bundle a local Microsoft-signed x64 Visual C++ package. By default the build uses a copy in the payload directory or downloads it from Microsoft.

```powershell
$env:FFMPEG_DIR    = "$PWD\tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared"   # has av1_nvenc
$env:LIBCLANG_PATH = "$PWD\.tmp\nvenc-vfr\clang18\clang\native"              # clang 18 for ffmpeg-sys
$env:CARGO_TARGET_DIR = "$PWD\target-dev"
cargo test -p recall -p recall-hdr -p recall-overlay --offline
$env:CARGO_TARGET_DIR = "$PWD\target-gpui"
cargo test --manifest-path recall-native\Cargo.toml --offline
```

Settings must survive updates and rollbacks. The `compatibility` tests (part of the runs above)
load the settings, watched folders, clip names and favourites that each earlier release wrote
(`compat/settings/`), and the release workflow checks that the previous release reads this build's
files. A change that cannot keep a setting ships only in a major release (x.0.0) that declares the
area it resets. The rule and how to follow it: [CONTRIBUTING.md](CONTRIBUTING.md), "Settings
compatibility".

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
versions. Tests that need NVENC or a live game are `#[ignore]`d; the release workflow runs
the rest on a GitHub-hosted runner.

Dev scripts that need this PC's values (monitor ids, clip folders, profile paths) read them from
`.local/machine.json` (gitignored): run `scripts\dev\init-machine.ps1 -List`, then
`init-machine.ps1` with the values; the template is `.local.example\machine.example.json`.
