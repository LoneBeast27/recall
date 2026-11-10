# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io/), certificate by [SignPath Foundation](https://signpath.org/).

## What is signed

Official Recall releases ship `RecallSetup.exe`, built from this repository's public `main` by
the [release workflow](.github/workflows/release.yml) on a GitHub-hosted runner. Every
executable Recall installs is signed, not only Setup:

1. The workflow builds `recall-native.exe`, `recall.exe`, `recall-hdr.exe`, `recall-overlay.exe`
   and `RecallUninstall.exe`, and checks that each carries the product name "Recall" and the
   release version.
2. Those five are submitted to SignPath only after a person approves the request in the
   `release-signing` environment.
3. Setup is built around the signed files, then submitted to SignPath, again only after approval.

After each round the workflow checks every Authenticode signature and timestamp before the
signed files are kept. A release tag must be on `main`. Nothing built outside that workflow is
signed. The SignPath artifact configurations are in [.signpath](.signpath/artifact-configurations).

## Roles

| Role | Member |
|---|---|
| Author (committer) | [LoneBeast27](https://github.com/LoneBeast27) |
| Reviewer | [LoneBeast27](https://github.com/LoneBeast27) |
| Approver (signing requests) | [LoneBeast27](https://github.com/LoneBeast27) |

## Privacy

Recall records and edits on your PC. Clips, screenshots, edits, the library and settings stay
on your computer; Recall has no account, telemetry or analytics. The only network traffic is:

- **Update check (the Recall app).** In an official versioned build, shortly after the app
  opens, it asks the GitHub API for the latest release
  (`https://api.github.com/repos/LoneBeast27/recall/releases/latest`) and, when that release is
  newer, compares its tag with `main`
  (`https://api.github.com/repos/LoneBeast27/recall/compare/...`). Turn this off with
  **Settings → General → Check for updates automatically**; then nothing is sent unless you
  press **Check now**. Development builds never check. Only when you choose to install an
  update does the app download `RecallSetup.exe` from the release on `github.com` (GitHub
  serves the file from `objects.githubusercontent.com` or `release-assets.githubusercontent.com`),
  check it against GitHub's SHA-256 digest, and open it. These requests go over HTTPS to those
  GitHub hosts only and carry the user agent `Recall-updater`; GitHub sees your IP address as
  for any download.
- **Setup opened by hand.** When Setup is opened directly (not by the app's update) over an
  existing install, it asks the GitHub API for the latest release and compares its tag with
  `main` (the same two requests as the app's update check, user agent `RecallSetup`) to tell you
  whether a newer version exists. It skips this when **Check for updates automatically** is off.
  Nothing about you is sent.
- **Setup.** The release Setup downloads the pinned FFmpeg 7.1.1 runtime from
  [Gyan's GitHub release](https://github.com/GyanD/codexffmpeg/releases/tag/7.1.1) and checks
  every file's SHA-256 (an update reuses an installed copy that still matches). When the
  Microsoft Visual C++ x64 runtime is missing, Setup downloads it from Microsoft
  (`https://aka.ms/vc14/vc_redist.x64.exe`) and runs it only if Microsoft's signature verifies.
  On Windows N editions it can add the Media Feature Pack through Windows Update (DISM).
- **Installing an update.** Before Setup replaces Recall, Recall saves any recording in progress
  and writes a small resume file in `%LOCALAPPDATA%\Recall\updates\` so it can reopen where you
  were, and Setup copies your settings into
  `%LOCALAPPDATA%\Recall\updates\config-backup-<version>-<time>\` (the last three are kept).
  These files stay on your PC; nothing about them is sent.
- **Links you click.** Opening the third-party notices from Settings opens this repository in
  your browser.

Nothing else is sent.
