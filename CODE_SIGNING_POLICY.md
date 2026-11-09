# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io/), certificate by [SignPath Foundation](https://signpath.org/).

## What is signed

Official Recall releases ship `RecallSetup.exe`, built from this repository's public `main` by
the [release workflow](.github/workflows/release.yml) on a GitHub-hosted runner. The workflow
submits that Setup to SignPath only after a person approves the request in the
`release-signing` environment, then checks the Authenticode signature and timestamp before the
signed file is kept. Nothing built outside that workflow is signed.

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
- **Setup.** The release Setup downloads the pinned FFmpeg 7.1.1 runtime from
  [Gyan's GitHub release](https://github.com/GyanD/codexffmpeg/releases/tag/7.1.1) and checks
  every file's SHA-256 (an update reuses an installed copy that still matches). When the
  Microsoft Visual C++ x64 runtime is missing, Setup downloads it from Microsoft
  (`https://aka.ms/vc14/vc_redist.x64.exe`) and runs it only if Microsoft's signature verifies.
  On Windows N editions it can add the Media Feature Pack through Windows Update (DISM).
- **Links you click.** Opening the third-party notices from Settings opens this repository in
  your browser.

Nothing else is sent.
