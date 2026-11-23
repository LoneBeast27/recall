# Known issues

Problems we know about in the current version of Recall, and what to do meanwhile. If yours is
not here, use **Report a problem** in Recall (see the [README](../README.md#report-a-problem)).

## Installing

- **Windows says "Windows protected your PC".** Recall's installer is not code-signed yet, so
  SmartScreen does not recognise the publisher. Choose **More info**, then **Run anyway**. See
  [Why does Windows warn me?](../README.md#why-does-windows-warn-me)
- **An antivirus flags a new release.** A new, unsigned installer can be flagged before it has
  built up a reputation. Check the file against `SHA256SUMS.txt` on the release page (see
  [Check a download yourself](../CODE_SIGNING_POLICY.md#check-a-download-yourself)) and please
  open an issue so we can report the false alarm.
- **Recall must be installed on an SSD.** Setup refuses a hard drive because the preview cache
  lives in the install folder. Your clips can still be saved to a hard drive.
- **Windows N editions** need the Media Feature Pack. Setup adds it through Windows Update; if it
  asks for a restart, open Setup again afterwards.

## Supported PCs

- **Windows 11 only.** Windows 10 is not supported: on Windows 10, clips miss the game's own
  sound. Linux and macOS are planned as more people ask for them.
- **NVIDIA cards only, for now**, from the GeForce GTX 10 series up. The GT 1030 and the MX
  laptop chips cannot record. AMD and Intel graphics are planned.
- **AV1 recording** needs an RTX 40 or RTX 50 card. Older cards record H.264 or HEVC instead.

## Recording

- **FPS stats show "No frame timing" right after installing.** The permission that lets Recall
  read frame timing takes effect at your next sign-in. Sign out and back in once.
- **A yellow border around a game in exclusive fullscreen.** On some PCs Windows does not let
  Recall hide its capture border while it records a game in exclusive fullscreen. The border is
  only on your screen, never in the clip. Setting the game to **Borderless** removes it.
- **A replay saved right after HDR turns on is short.** When the display switches from SDR to HDR
  during a game, the replay buffer starts again from that moment.
- **A game is not recorded.** Recall records only apps it can tell are games. Add the game in
  **Settings → Games**, or from the tray menu after the "Not recording" notice.

## Library

- **A clip renamed or moved outside Recall loses its favourite and title.** Recall remembers
  these by the clip's location. Rename clips inside Recall to keep them.
- **A USB drive with linked clips may not eject.** Close Recall before you choose **Safely
  remove** for that drive.
- **HDR display switching is turned off.** Recall used to be able to turn your monitor's HDR on
  for HDR games. That feature is paused; recording an HDR desktop still works and is saved as a
  normal (SDR) clip.
