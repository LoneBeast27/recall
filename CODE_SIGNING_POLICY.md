# Code signing policy

Recall releases are currently **not code-signed**. Recall applied to the free SignPath
Foundation program for open-source projects, which asked us to reapply once the project is
more widely known. Until a release is signed, Windows treats `RecallSetup.exe` as coming from
an unknown publisher.

## What you will see

- **SmartScreen** may show "Windows protected your PC". Choose **More info**, check that the
  file is `RecallSetup.exe` from this repository's releases, then **Run anyway**.
- **Microsoft Defender** or another antivirus may flag a new, unsigned release before it has
  built up reputation. Each release is reported to Microsoft as a false positive when that
  happens. Download only from [the Releases page](https://github.com/LoneBeast27/recall/releases).

## Check a download yourself

Every release lists the installer's SHA-256 in `SHA256SUMS.txt` and the exact source commit in
`SOURCE-REVISION.txt`. In PowerShell:

```powershell
Get-FileHash .\RecallSetup.exe -Algorithm SHA256
```

The hash must match the `RecallSetup.exe` line in `SHA256SUMS.txt`. Recall's own updater does
this check automatically before it opens Setup.

## How releases are built

Official releases are built from this repository's public `main` by the
[release workflow](.github/workflows/release.yml) on a GitHub-hosted runner, never on a
personal PC. The workflow builds `recall-native.exe`, `recall.exe`, `recall-hdr.exe`,
`recall-overlay.exe` and `RecallUninstall.exe`, checks that each carries the product name
"Recall" and the release version, and builds Setup around them. A release tag must be on
`main`.

The workflow already contains signing steps (SignPath configurations are in
[.signpath](.signpath/artifact-configurations)). They stay switched off until a signing
certificate is in place; when that happens, every executable will be signed, not only Setup,
each signing request will need a person's approval, and this page will say so.

## Roles

| Role | Member |
|---|---|
| Author (committer) | [LoneBeast27](https://github.com/LoneBeast27) |
| Reviewer | [LoneBeast27](https://github.com/LoneBeast27) |
| Approver (releases and future signing requests) | [LoneBeast27](https://github.com/LoneBeast27) |

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
