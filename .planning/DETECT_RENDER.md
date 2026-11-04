# Detection: unknown fullscreen apps must render like a game (2026-11-05)

Branch `fix/detect-render`, based on `fix/detect-deny` (947087b, not on master yet).

## Rule

An unknown fullscreen window (not in a Steam library, no `hdr_game`/`sdr_game` override, not denied, not pinned)
commits as a game only if, while it waits for its start debounce, its own process shows one of:

| signal | source | threshold |
|---|---|---|
| exclusive fullscreen | DxgKrnl `Flip_Info` (168) on its threads | 3 legacy flips |
| independent flip | Win32k `TokenStateChanged` (301) `InFrame` + `IndependentFlip` on its surfaces (201) | 3 frames and at least half of its on-screen frames |
| tearing presents | DXGI `Present_Start` (42) `Flags & DXGI_PRESENT_ALLOW_TEARING` | 3 presents and at least half of its presents |

It is rejected when its presents are mostly DXGI `PresentMultiplaneOverlay` (55, decode swap chains = video), or when
1.5 s pass without a game signal (composed by DWM only, or no frames). A rejected window is not a candidate again for
10 s, then 20, 40, 80 s (cap) while it stays rejected.

Steam-library exes, overrides and pinned targets keep the 300 ms rule with no check. Without trace rights the
check is off and today's rule applies (fullscreen + not denied = game); the log says so once.

## Evidence

- VALORANT, PresentMon 2026-11-01 with Recall's island up (`.tmp/valorant-ab-2026-11-01/on`, 336 s):
  244,629 frames `Hardware: Independent Flip`, 12 `Hardware Composed: Independent Flip`, 39,652 `Composed: Flip`
  (the stretches where Rohan had left the game window). All 284,293 DXGI presents: SyncInterval 0, Flags 0x200
  (`ALLOW_TEARING`). Recall OFF run: 218,127 independent flip, 17 composed. So the island does not stop
  independent flip, and a foreground game is independent-flip from its first second of play.
- This PC's desktop, 10 s system-wide trace 2026-11-05 01:32 IST (DXGI, D3D9, DxgKrnl Present keyword, Win32k
  Updates; nothing opened or shown): 5,055 Win32k composition tokens from 2 presenting processes, all composed
  (`IndependentFlip=false` on 100% of 20,224 state changes); 0 `Flip_Info`; Brave (a YouTube tab), Discord,
  steady-mic: no DXGI presents in 10 s.
- Not measured (nothing opened on Rohan's screens, per the brief): Netflix / Photos / Movies & TV / Media
  Player fullscreen, Spotify, PowerPoint slideshow, Resolve, Premiere, Plex, Kodi, Teams/Zoom share, any 60 Hz
  laptop game. The expectations for them below are from PresentMon's present-mode model, not from this PC.

## Expected per class (unverified unless marked)

| app | expected path | verdict |
|---|---|---|
| fullscreen game, flip model | independent flip (measured: VALORANT) | game |
| exclusive fullscreen game | legacy flip | game |
| game composed because a window sits above it | composed, tearing presents if the game allows tearing | game only with tearing |
| UWP video (Netflix, Movies & TV, Media Player) | decode swap chain (55) or composed | not a game; an independent-flip video without 55 would still count |
| Discord, Spotify, Teams, Zoom share (Chromium/Electron) | composed flip | not a game |
| PowerPoint slideshow, Photos | composed or no frames | not a game |
| Resolve, Premiere, Plex, Kodi | unknown; Kodi renders like a game (D3D11 swap chain, may get independent flip) | Kodi likely a game |

## Known gaps

- A borderless game that DWM composes and that does not allow tearing (old blt-model DX9/DX11 with Windows'
  windowed-game optimizations off, native Vulkan/OpenGL without the DXGI layer, a third-party overlay window on
  top) is rejected. Fix for the user: an `sdr_game` / `hdr_game` override, or a Steam-library install.
- A fullscreen video that gets independent flip without a decode swap chain is still recorded (today's
  behaviour).
- The HDR manager's own foreground rules are unchanged (they decide HDR switching, not recording).

## Cost and latency (measured 2026-11-05 on this PC, gated, hidden window, no audio)

`cargo test -p recall-hdr live_probe -- --ignored --nocapture --test-threads=1`:

| what | number |
|---|---|
| session start on the pump thread (StartTrace, 4 providers, consumer thread) | 0.38-0.48 ms (5 runs) |
| session stop on the pump thread (stop + join) | 0.86-1.06 ms (5 runs) |
| present -> counted in the callback | 64-115 ms (5 presents; 50 ms flush timer) |
| this process's CPU, presenting ~620 fps, probe off / on / off | 13.9 % / 15.4 % / 12.5 % of one core |
| live check: 60 hidden tearing presents | 60 runtime, 60 tearing, 60 kernel, 60 DWM frames (composed), 0 video, 0 legacy |
| session still running after drop | no (query by name: not found), every run |

So watching a busy candidate costs about 1.5-3 points of one core on a 9950X3D, for at most 1.5 s per candidate
(only while it is pending). A rejected window is watched again after 10, 20, 40, 80 s, so a fullscreen video left
open costs one 1.5 s watch per 80 s at steady state. Scaling to a GTX 1650 laptop's CPU is not measured.

Latency for a real game, by construction (no game was run): before = 300 ms (start debounce). After = 300 ms when
3 independent-flip frames have arrived by then (VALORANT at 800 fps: its first ~200 ms of frames), else the next
100 ms ask; a 30 fps borderless game: about 300-400 ms. Upper bound for any game: 1.5 s.
