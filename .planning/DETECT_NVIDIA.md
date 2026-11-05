# Detection: NVIDIA's game list skips the render check (2026-11-05)

Branch `fix/detect-nvidia`, on `fix/detect-render` (render check + `[detect] fullscreen_games`).

## Rule

After the deny lists pass a fullscreen window (`record_target::is_target`), and only when the render check would
run (not a Steam-library exe, no override, not in `detect.fullscreen_games`, trace rights present), Recall asks
the GPU vendor whether the exe is a game. NVIDIA says yes → the window commits on the 300 ms rule with no render
check. That also records composed borderless games the check would reject (the known gap in DETECT_RENDER.md),
if NVIDIA lists them. NVIDIA says no, or has not answered yet → the render check decides as before; the check's
admit step asks again, so an answer that arrives while the candidate waits still commits it.

Vendor code is its own crate: `recall-nvidia` (NvAPI DRS, read-only). `recall-hdr/src/gpu_games.rs` holds the
seam (`GameKnowledge` trait, `open_vendor`); AMD / Intel crates plug in there.

## Signals compared (all read headlessly on Rohan's PC; no app opened)

### a) Driver profile DB (NvAPI DRS) — chosen

`NvAPI_DRS_CreateSession` + `LoadSettings` + `FindApplicationByName(full exe path)` + `GetSetting(0x105E2A1D)`.

Having a profile means nothing: 7,984 profiles, and every common non-game has a predefined one. The game marker
is setting `0x105E2A1D` (the driver gives it no name; Ansel test apps carry it, so it is Ansel-related). A
non-zero value = game.

| exe | driver profile | marker 0x105E2A1D | verdict |
|---|---|---|---|
| VALORANT.exe / VALORANT-Win64-Shipping.exe | Valorant | 4 | game |
| RocketLeague.exe | Rocket League | 4 | game |
| cs2.exe | Counter-strike 2 | 4 | game |
| r5apex_dx12.exe | Apex Legends | 4 | game |
| chrome.exe | Google Chrome | — | not a game |
| msedge.exe | Microsoft Edge Beta | — | not a game |
| firefox.exe | Mozilla Firefox | — | not a game |
| brave.exe | Brave | — | not a game |
| Discord.exe | Discord | — | not a game (kept a game by `fullscreen_games`) |
| Photoshop.exe | Adobe Photoshop | — | not a game |
| Adobe Premiere Pro.exe | Adobe Premiere | — | not a game |
| Resolve.exe | DaVinci Resolve | — | not a game |
| Spotify.exe | Spotify | — | not a game |
| ms-teams.exe / Teams.exe | Microsoft Teams | — | not a game |
| Zoom.exe | Zoom | — | not a game |
| POWERPNT.EXE | Microsoft Powerpoint | — | not a game |
| vlc.exe | VLC media player | — | not a game |
| steam.exe, lghub.exe, obs64.exe, explorer.exe, notepad.exe, blender.exe, Code.exe | own profiles | — | not a game |
| mpv.exe, kodi.exe, Plex.exe, recall-native.exe | no profile | — | not a game |

- Whole DB: 1,470 profiles have the marker non-zero (1,299 at 4, 171 at 1). A name scan for player / studio /
  office / adobe / browser / media / launcher / tool / viewer / editor found only games and Ansel test apps.
  9 profiles hold 0, among them Wallpaper Engine (a non-game), Fortnite, MSFS 2024, RUST.
- Rohan's 60 installed apps (from the NVIDIA app's scan, by full path): 40 of 45 games marked; 0 of 15 non-games
  (VLC, Discord, Steam, G Hub, OBS, Streamlabs, Ollama, LM Studio, Blender, Cinema 4D, Adobe apps, 3DMark) marked.
  Unmarked games: KovaaK's, 2XKO and Assassin's Creed Origins (no profile; Origins not installed at its path), Fortnite (0), Arc Raiders (profile without marker), Cities:
  Skylines and Rematch match only by full path (folder rules), so the lookup sends the full path.
- Cost (this PC, driver DB 1.9 MB): NvAPI_Initialize 1.7 ms; CreateSession 0.05 ms; LoadSettings 103-132 ms
  (5 runs); FindApplicationByName 4-87 µs on a hit, 0.3-1.3 ms on a miss (one 53 ms folder-rule hit); a loaded
  session holds about 18 MiB private, freed on destroy.
- Needs only the NVIDIA driver (nvapi64.dll), not the NVIDIA app.

### b) NVIDIA app data (`%LOCALAPPDATA%\NVIDIA Corporation\NVIDIA App\NvBackend`) — not used

- `ApplicationStorage.json`: the app's scan of this PC, 78 entries: Rohan's games (VALORANT, Rocket League ×3
  stores, CS2, Apex, …) plus Discord, VLC, Steam, Logitech G Hub, OBS, Streamlabs, Ollama, LM Studio, Blender,
  Adobe apps. A per-app `IsCreativeApplication` flag marks most non-games, but Steam and G Hub are `false`.
- `ApplicationOntology/data/fingerprint.db`: the full catalog, XML, 2,078 fingerprints, 87 ms to parse. 105 are
  `IsCreativeApplication`; of the rest, about 30 are non-games (Steam, Logitech G Hub, NVIDIA Broadcast / Canvas,
  Omniverse apps, DouYin, HuYa, Bilibili, DingTalk, Evernote, Roblox Studio, PRISM Live Studio, …). Not listed:
  Chrome, Edge, Firefox, Brave, Spotify, Teams, Zoom, PowerPoint, mpv, Kodi, Plex.
- Undocumented, per-user, written by the NVIDIA app's backend (present without the app running, absent without
  the app installed). Field names changed across app versions (`IsCreativeApplication` is recent).

Why (a): it ships with every NVIDIA driver, it is a stable public API, and its marker flagged no non-game in the
whole DB, while (b) lists real non-games as non-creative and needs the NVIDIA app. The marker ID itself is
undocumented: if a driver drops it, nothing is a known game and today's rules apply.

## Cost on the record path

- Asked only for a fullscreen candidate that the deny lists passed and the render check would watch.
- One lookup per new exe path, on a lazy worker thread (`gpu-games`, blocking receive, no polling); the pump
  thread never waits. One session per batch (~110 ms CPU on the worker, ~18 MiB for that time), then destroyed.
  Verdicts are cached per path for the controller's life; pid → path is cached per pid.
- No NVIDIA driver: one log line (`GPU game list off (…)`), every answer "not known", today's rules.

## Not verified

- No game was launched (brief: nothing opened on Rohan's screens). The commit path is covered by unit tests and
  the live lookup test; the end-to-end log line (`render check skipped: NVIDIA lists …`) is unobserved.
- Non-NVIDIA PCs: covered by the `no_vendor_means_never_a_known_game` test with a stubbed opener, not on hardware.
