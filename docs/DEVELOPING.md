# Developing Recall

This page is for people who build Recall from source, change its code or make releases. To
install and use Recall, see the [README](../README.md) and the [user guide](user-guide.md).
Contributions follow [CONTRIBUTING.md](../CONTRIBUTING.md).

## Source and license

Recall was created by **LoneBeast27**. Official Recall downloads are free. Recall’s own code is released under [GPL-3.0-or-later](../LICENSE), which allows commercial use and redistribution. If you distribute a modified build, keep the copyright and license notices and clearly mark your changes and their dates. The [upstream Recall repository](https://github.com/LoneBeast27/recall) is the reference for the original source. See [NOTICE.md](../NOTICE.md) for credits and third-party terms.

## Parts

| Folder | Program | Job |
|---|---|---|
| `recall/` | `recall.exe` | capture engine: Windows Graphics Capture → NVENC → memory ring; saves, exports, long recordings |
| `recall-hdr/` | `recall-hdr.exe --record` | controller + tray: finds the game, runs the engine, hotkeys, HDR switching, config, control pipe `\\.\pipe\recall-hdr-control-<session id>` (this user and SYSTEM only) |
| `recall-overlay/` | `recall-overlay.exe` | the small in-game save notice and two-step shortcut hint |
| `recall-native/` | `recall-native.exe` | the app (GPUI): library, editor, HDR, audio, settings |
| `recall-hub/` | — | the old Svelte/Tauri window; replaced by `recall-native`, kept for reference |

## Install from source

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

## Rehearse setup (RecallTesting)

Run `dist/recall-testing/RecallTesting.exe` to rehearse setup with demo drives, audio sources, and a demo library. Changes stay in memory for that session. The testing executable has no connection to the recorder and uses no Recall profile or clip files.

To build and install Recall from source, run this in the repo:

## Build and test

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
area it resets. The rule and how to follow it: [CONTRIBUTING.md](../CONTRIBUTING.md), "Settings
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

## Releases

Feature releases are built by the [release workflow](../.github/workflows/release.yml) on a GitHub-hosted runner, as described in the [code signing policy](../CODE_SIGNING_POLICY.md) (releases are currently unsigned). `scripts\release.ps1 -Version <major.minor.patch> -ArtifactDirectory <extracted CI bundle>` checks that bundle (release tag on `main`, source revision, SHA-256, version) and refuses an unsigned Setup once signing is configured. Urgent hotfixes, for bugs that affect users right now, may be built locally from the tagged public source: run `powershell -NoProfile -File scripts\release.ps1 -Version <major.minor.patch> -FfmpegArchive <zip> -FfmpegDirectory <bin folder> -VcRedist <vc_redist.x64.exe>` from a clean checkout at the `v<major.minor.patch>` tag on `main` (`Get-Help scripts\release.ps1 -Full` lists every step). It runs the tests, strips local paths from the build, refuses any build that still contains them, and scans the result with Defender. Pushing the tag still runs the workflow as an independent re-check. Recall 0.1.1 was built locally from its tagged source with `scripts\release.ps1`; its `SOURCE-REVISION.txt` names that commit. Every release's `SOURCE-REVISION.txt` names the commit it was built from. The output contains `RecallSetup.exe`, `SHA256SUMS.txt`, `SOURCE-REVISION.txt`, and the Recall executables in `recall-app` for individual scans. A draft can be verified before publishing. A same-version hotfix keeps the version number: move the `v<major.minor.patch>` tag to the fixed commit on `main`, build from it, and replace the release's `RecallSetup.exe`, `SHA256SUMS.txt` and `SOURCE-REVISION.txt`. Release builds carry their source commit, so the app offers the hotfix once at start-up (as "Hotfix for <version>") and stays quiet once it is installed; Setup run by hand installs it over the earlier build of that version and says "already the latest" for the same build. Copies of 0.1.5 installed before this change cannot see a same-version hotfix: run the new Setup once by hand.

Run `Get-Help scripts/dev/scan-release.ps1 -Examples` for a per-file Defender scan of the installer, app binaries, and vendor downloads.

## Experimental HDR switching

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

## FPS history

An earlier build ran a private RTSS copy for frame timing. Exiting RTSS while a game had its hook loaded crashed VALORANT, so it was removed on 2026-10-26; frame timing now comes from ETW only.
