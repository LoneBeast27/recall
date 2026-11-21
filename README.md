# Recall

**Save the play you just made.** Recall runs quietly while you game, keeps the last couple of
minutes in memory, and saves them as a clip when you press **Alt+N**. Then trim, cut and share
the clip from a clean library and editor, all on your own PC.

Free, no account, no ads, nothing uploaded. Windows with an NVIDIA graphics card.

### [⬇ Download Recall for Windows](https://github.com/LoneBeast27/recall/releases/latest/download/RecallSetup.exe)

Latest version, installer about 15 MB. [All releases and what changed](https://github.com/LoneBeast27/recall/releases)

<!-- SCREENSHOT: library with made-up clips (docs/images/library.png) -->
<!-- SCREENSHOT: editor with a trimmed clip (docs/images/editor.png) -->
<!-- SCREENSHOT: the in-game pill over a game (docs/images/island.png) -->

## What it does

- **Instant replays.** Recording starts by itself when a game is in front and stops when it
  closes. Press **Alt+N** to save the last 2 minutes (you can change the length). A chime tells
  you the clip is safe.
- **More shortcuts.** Press **Alt + `** (the key left of 1), then **1** to add a bookmark, **2**
  to save the last 30 seconds, **3** for a screenshot, or **4** to start or stop a full recording.
  **Alt+M** mutes your microphone in clips.
- **A small in-game pill.** It shows when a game is picked up and when a clip is saved. Turn on
  live stats to see your FPS, its 1% low, and GPU and CPU use. It keeps clear of each game's HUD.
- **A library that stays tidy.** Clips are grouped by day. Hover a clip to scrub through it.
  Favourite, rename, drag a clip straight into Discord, or delete it to the Recycle Bin.
- **A simple editor.** Trim, split and cut, set the volume of each sound track, slow down or
  speed up a part, then export. Pick a size limit (such as 10 MB for Discord) and Recall fits the
  clip into it. Your original clip never changes.
- **Separate sound tracks.** Game sound, your microphone and voice chat are kept apart, so you can
  turn one down later.
- **Bring your old clips.** Link a ShadowPlay, Medal or OBS folder and those clips show up in the
  library too, left where they are.
- **Private by design.** Clips, edits and settings stay on your PC. Recall has no account,
  telemetry or analytics. [What Recall connects to](docs/user-guide.md#privacy)

## Install

1. [Download `RecallSetup.exe`](https://github.com/LoneBeast27/recall/releases/latest/download/RecallSetup.exe).
2. Open it. If Windows shows **"Windows protected your PC"**, choose **More info**, then
   **Run anyway** (see [Why does Windows warn me?](#why-does-windows-warn-me) below).
3. Follow Quick Setup. It picks a place for Recall on your SSD and sets up anything Windows is
   missing. Windows may ask for administrator permission once.
4. Choose **Open Recall**, start a game, and press **Alt+N** after a good moment.

Recall starts recording games on its own. Its icon sits in the system tray: red while
recording, grey while paused.

### System requirements

| | |
|---|---|
| Windows | Windows 11, 64-bit. Windows 10 (64-bit) records video and your microphone, but game sound needs an extra step: see [Known issues](docs/known-issues.md#recording). |
| Graphics | NVIDIA GeForce GTX 10 series or newer (GTX 1050 and up, GTX 16, RTX 20 to RTX 50). The GT 1030 and MX laptop chips cannot record. |
| Storage | Recall installs on an SSD. Clips can go on any drive, a hard drive included. |

AMD and Intel graphics are not supported yet.

### Updates

Recall checks for a new version shortly after it opens and asks before it installs anything.
Your clips, settings and favourites are kept. You can also check yourself in
**Settings → Help → About**, or turn the automatic check off there.

## FAQ

### Why does Windows warn me?

Recall's installer is not code-signed yet. Code signing is a paid certificate that tells Windows
who published a file. Without it, Windows SmartScreen does not recognise the publisher and asks
you to confirm. This is normal for small, new apps. Choose **More info**, check that the file is
`RecallSetup.exe`, then **Run anyway**.

Download Recall only from this page. Each release lists the installer's fingerprint
(`SHA256SUMS.txt`), and Recall's own updater checks it for you. To check a download by hand, see
[Check a download yourself](CODE_SIGNING_POLICY.md#check-a-download-yourself).

### Which games does it record?

Games from Steam, Epic, GOG, Ubisoft Connect, the EA app, Battle.net, itch and the Xbox app are
picked up on their own, as are games the NVIDIA driver knows. If a game is not recorded, Recall
tells you after a short while: add it in **Settings → Games**, or from the tray menu. You can
also mark an app **Never record**. [More detail](docs/user-guide.md#what-counts-as-a-game)

### Does it slow my game down?

Recall uses your graphics card's built-in video encoder (NVENC), the same part that ShadowPlay
uses, so the game keeps the processor to itself. Recall never injects anything into the game: it
records the screen the way Windows' own capture does.

### Where are my clips?

In `Videos\Recall` unless you choose another folder in **Settings → Storage**. Screenshots go in
a `screenshots` folder inside it. Set a storage limit there and the oldest clips go to the Recycle
Bin; favourites and edited clips are always kept.

### Can I record my desktop or an app that isn't a game?

Yes. Add the app in **Settings → Games** and Recall records it in any window. You can also have
Recall keep recording the desktop between games.

### How do I uninstall it?

Open Windows **Settings → Apps → Installed apps**, find **Recall** and choose **Uninstall**. Your
clips stay where they are.

## Known issues

See [Known issues](docs/known-issues.md) for problems we know about and what to do meanwhile.

## Report a problem

In Recall, open **Report a problem** from the sidebar. Describe what happened and Recall saves a
small zip with an anonymous record of what the app did (never your clip names, files or window
titles). You can then open a new
[GitHub issue](https://github.com/LoneBeast27/recall/issues/new) with your description and attach
the zip. Recall never uploads anything by itself.

---

**More:** [User guide](docs/user-guide.md) · [What's new](CHANGELOG.md) ·
[Code signing policy](CODE_SIGNING_POLICY.md) · [Credits and licenses](NOTICE.md) ·
[For developers](docs/DEVELOPING.md)

Recall is made by [LoneBeast27](https://github.com/LoneBeast27) and is free and open source under
[GPL-3.0-or-later](LICENSE).
