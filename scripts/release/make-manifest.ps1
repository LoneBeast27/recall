<#
.SYNOPSIS
  Writes the UNSIGNED update manifest (recall-update.unsigned.json) for a release bundle.
.DESCRIPTION
  The bundle folder holds RecallSetup.exe and SOURCE-REVISION.txt (release.ps1 or release.yml
  output). The manifest names Setup's size and SHA-256 and every download address, in the
  app's tier order: GitLab, the update site on the maintainer's domain, GitHub. Nothing is
  signed or uploaded here: sign-manifest.ps1 signs it with the PC offline.
.EXAMPLE
  .\make-manifest.ps1 -Bundle .tmp\release\0.1.11 -Version 0.1.11 -NotesFile notes.txt `
      -GitLabProject example/recall-updates -GitLabProjectId 123 -UpdateSite updates.example.org
#>
param(
    [Parameter(Mandatory)][string]$Bundle,
    [Parameter(Mandatory)][ValidatePattern('^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$')][string]$Version,
    [Parameter(Mandatory)][ValidatePattern('^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$')][string]$GitLabProject,
    [Parameter(Mandatory)][ValidatePattern('^[0-9]+$')][string]$GitLabProjectId,
    [Parameter(Mandatory)][ValidatePattern('^[a-z0-9.-]+$')][string]$UpdateSite,
    [string]$NotesFile,
    [string]$Title,
    # Same-version builds this hotfix replaces (their 40-hex source commits).
    [string[]]$Supersedes = @(),
    [string]$MinVersion,
    # GitHub is down: leave its address out so the app does not spend a try on it.
    [switch]$SkipGitHub
)
$ErrorActionPreference = 'Stop'
if (Test-Path -LiteralPath (Join-Path $Bundle 'TEST-BUILD.txt')) { throw 'This is a test build (no update keys): it can never be released.' }
$setup = Join-Path $Bundle 'RecallSetup.exe'
$revision = (Get-Content -LiteralPath (Join-Path $Bundle 'SOURCE-REVISION.txt') -TotalCount 1).Trim().ToLowerInvariant()
if ($revision -notmatch '^[0-9a-f]{40}$') { throw "SOURCE-REVISION.txt does not hold a commit: $revision" }
foreach ($r in $Supersedes) { if ($r -notmatch '^[0-9a-f]{40}$') { throw "Not a commit: $r" } }
$file = Get-Item -LiteralPath $setup
$sha = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
$urls = @(
    "https://gitlab.com/api/v4/projects/$GitLabProjectId/packages/generic/recall/$Version/RecallSetup.exe",
    "https://$UpdateSite/v$Version/RecallSetup.exe"
)
if (!$SkipGitHub) { $urls += "https://github.com/LoneBeast27/recall/releases/download/v$Version/RecallSetup.exe" }
$manifest = [ordered]@{
    schema      = 1
    channel     = 'stable'
    # Only goes up: the UTC minute the manifest was made (the app refuses an older one).
    sequence    = [int64](Get-Date).ToUniversalTime().ToString('yyyyMMddHHmm')
    version     = $Version
    revision    = $revision
    supersedes  = @($Supersedes | ForEach-Object { $_.ToLowerInvariant() })
    published   = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    min_version = if ($MinVersion) { $MinVersion } else { $null }
    title       = if ($Title) { $Title } else { "Recall $Version" }
    notes       = if ($NotesFile) { Get-Content -LiteralPath $NotesFile -Raw } else { '' }
    setup       = [ordered]@{ name = 'RecallSetup.exe'; size = $file.Length; sha256 = $sha; urls = $urls }
    revoke_keys = @()
}
$out = Join-Path $Bundle 'recall-update.unsigned.json'
# UTF-8 without a byte-order mark: the signature covers these exact bytes.
[IO.File]::WriteAllText($out, ($manifest | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
"Wrote $out (sequence $($manifest.sequence), Setup $($file.Length) bytes, SHA-256 $sha)"
"GitLab manifest path: https://gitlab.com/$GitLabProject/-/raw/main/recall-update.json"
