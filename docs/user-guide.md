# Recall user guide

The full details behind the [README](../README.md): what Recall records, every shortcut, the
settings, where files are kept, and exactly what Recall connects to.

## Setup and updates

Open `RecallSetup.exe` for Quick Setup. Choose whether recording starts when setup is complete and whether to set up Windows game detection and FPS. **Change…** opens the SSD install folder picker from Quick Setup. **Advanced** also lets you type a folder or change the Start menu shortcut. After installation, **Continue setup** opens Storage, Audio, and Recording quality. Recording quality compares the same full frame before and after encoding, with a detail crop available. Choose **Open Recall** to reach your library. Updates keep your existing clip folder and recording settings.

Setup checks the PC before copying Recall. Release Setup downloads the pinned FFmpeg runtime from Gyan and the signed Microsoft Visual C++ x64 runtime when required. On Windows N, it adds the Media Feature Pack through Windows Update when needed. Windows may ask for administrator permission. If a restart is needed, reopen Setup after restarting. Game recording uses the NVIDIA driver installed for your GPU.

Official versioned builds check [Recall's GitHub Releases](https://github.com/LoneBeast27/recall/releases) after launch. You can also check in **Settings → Help → About**. Recall offers a newer stable release once it verifies that the tag is on `main`. It verifies the Setup download against GitHub's SHA-256 before opening Setup for confirmation.

## Using Recall

- **Play a game.** Recording starts by itself when a game is in front and stops when it
  closes. The tray icon is red while recording and grey while paused.

### What counts as a game

Recall records an app on its own only with official proof that it is a game:

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

### Saving clips and shortcuts

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

### The in-game pill (Recall Island)

- **Recall Island** (the in-game overlay): a small pill that drops in when a game is picked up
  ("Recording VALORANT"), when recording switches to the desktop, pauses or stops, and for saves,
  bookmarks and screenshots. **Alt + ` then 5** (or the gauge button in the app's title bar, or
  Settings → General) toggles live performance stats: FPS with its 1% low, GPU and CPU rings,
  and the recording dot. Each game gets its own spot, measured from your clips so the Island
  stays clear of that game's HUD (`hud-spots.json`); unknown games use the top right.
  The pill uses compact spacing and stacks text upright along a side edge.
  The overlay shortcut (Alt+O by default) hides or shows notices and live stats together.

### Tray menu and library

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

## Sound sources

Fresh profiles use the Windows default microphone and system output. A distinct Windows communications output gets its own track. Add extra devices or apps from **Input**, **Output**, and **Application audio**. Select the icon on a custom strip to change it.

## FPS numbers (Windows frame timing)

FPS and 1% low come from Windows' own present events (ETW), the method Intel PresentMon and
NVIDIA FrameView use: nothing is injected into games and no extra tool runs. While stats are on,
the overlay keeps a small trace session ("RecallFrameTiming") on the game's process; it stops
when stats turn off. Reading these events needs the "Performance Log Users" group: the installer
adds you through one administrator prompt (`-NoFrameTiming` skips it), and it takes effect at
your next sign-in. Until then the Island shows "No frame timing".

Recall does not use RTSS or any other overlay tool for these numbers.

## Settings

| Tab | What it holds |
|---|---|
| General | Start with Windows, Island notices, performance stats |
| Recording | automatic recording, desktop between games, replay length, format (H.264 / HEVC / AV1), frame rate, resolution, bitrate, voice chat on its own track |
| Storage | clip folders, linked folders, storage limit (oldest clips go to the Recycle Bin; favourites, edited clips, recently played clips and the clip open in the app stay), SSD cache size |
| Games | the apps you added, Never record entries, apps recorded only when fullscreen |
| Shortcuts | the in-game shortcuts and the two-step switch |
| Help | fixes Recall can make for you, Report a problem, About and updates |

The Audio page shows live meters for every source.

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

## Privacy

Clips, screenshots, edits, the library and settings stay on your PC; Recall has no account,
telemetry or analytics. The only network traffic is:

- **Update check:** official versioned builds ask the GitHub API for the latest release
  (`api.github.com/repos/LoneBeast27/recall/releases/latest`, then a compare of its tag with
  `main`) shortly after the app opens. Turn it off with **Settings → Help → Check for updates
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
- **Action trace and crash records** stay in `%LOCALAPPDATA%\Recall\trace\` on your PC. They
  hold only what Recall did (action names, states, counts, timings, error codes), never paths,
  window titles, clip, program or device names. Nothing is sent.
- **Report a problem** saves a zip on your PC and, if you choose, opens a new GitHub issue in
  your browser with your description; you attach the zip yourself. The zip holds the anonymous
  action trace, a settings summary of numbers and switches, and screenshots, clips, sound or
  text files you add (text is sent as written; other files lose their metadata); never Recall's
  developer logs or names. Recall uploads nothing.

Nothing else is sent. Details: [Privacy](../CODE_SIGNING_POLICY.md#privacy).

