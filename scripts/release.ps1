<#
  Cuts a Recall release that the in-app updater can offer (services/update.rs).

  Release gating (Rohan, 2026-11-06): day-to-day work lives on `dev` and never becomes an
  update. A release is cut only from `main`, after Rohan approves the build number with its
  list of features and patches. This script refuses anything else.

  Usage (from the repo, on a clean `main`):
    powershell -NoProfile -File scripts\release.ps1 -Version 1.0.0 -Notes .planning\release-1.0.0.md            # build + zip only
    powershell -NoProfile -File scripts\release.ps1 -Version 1.0.0 -Notes .planning\release-1.0.0.md -Draft     # + draft release (updater cannot see it)
    powershell -NoProfile -File scripts\release.ps1 -Version 1.0.0 -Notes .planning\release-1.0.0.md -Publish   # + live release (offered to installs)

  Output: .tmp\release\<version>\Recall-<version>-win64.zip and SHA256SUMS.txt. The build stamps
  RECALL_VERSION into the app; dev builds carry none and never check for updates.
#>
param([Parameter(Mandatory)][string]$Version, [Parameter(Mandatory)][string]$Notes, [switch]$Draft, [switch]$Publish)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
Push-Location $repo
try {
  if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "version must be MAJOR.MINOR.PATCH (got '$Version')" }
  if (-not (Test-Path $Notes)) { throw "release notes file not found: $Notes" }
  $branch = (git rev-parse --abbrev-ref HEAD).Trim()
  if ($branch -ne 'main') { throw "releases are cut from main only (on '$branch'); dev work stays on dev" }
  $dirty = git status --porcelain --untracked-files=no
  if ($dirty) { throw "main has uncommitted changes; commit or stash them first" }
  if (git tag --list "v$Version") { throw "tag v$Version already exists" }
  $sha = (git rev-parse --short HEAD).Trim()

  $out = Join-Path $repo ".tmp\release\$Version"
  $stage = Join-Path $out 'stage'
  if (Test-Path $out) { Remove-Item -LiteralPath $out -Recurse -Force }
  New-Item -ItemType Directory -Force -Path $out | Out-Null

  $env:RECALL_VERSION = $Version
  & powershell -NoProfile -File (Join-Path $PSScriptRoot 'install.ps1') -StageOnly -StageDir $stage
  if ($LASTEXITCODE -ne 0) { throw "build failed (exit $LASTEXITCODE)" }

  $zipName = "Recall-$Version-win64.zip"
  $zip = Join-Path $out $zipName
  Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal
  $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLowerInvariant()
  $sums = Join-Path $out 'SHA256SUMS.txt'
  Set-Content -Path $sums -Value "$hash  $zipName" -NoNewline
  $mb = [math]::Round((Get-Item $zip).Length / 1MB)
  "Built Recall $Version from main $sha : $zip ($mb MB), sha256 $hash"

  if ($Draft -or $Publish) {
    git tag -a "v$Version" -m "Recall $Version" | Out-Null
    git push origin "v$Version"
    if ($LASTEXITCODE -ne 0) { throw "pushing tag v$Version failed" }
    $flags = @('release', 'create', "v$Version", $zip, $sums, '--title', "Recall $Version", '--notes-file', $Notes, '--verify-tag')
    if (-not $Publish) { $flags += '--draft' }
    gh @flags
    if ($LASTEXITCODE -ne 0) { throw "gh release create failed" }
    if ($Publish) { "Published: installs offer Recall $Version on their next start." } else { "Draft created: not offered until published." }
  }
} finally {
  Pop-Location
}
