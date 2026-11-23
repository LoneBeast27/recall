<#
.SYNOPSIS
  Writes the UNSIGNED update manifest (recall-update.unsigned.json) for a release bundle.
.DESCRIPTION
  The bundle folder holds RecallSetup.exe and SOURCE-REVISION.txt (release.ps1 or release.yml
  output). The manifest names Setup's size and SHA-256 and every download address, in the
  app's tier order: GitLab, the update site on the maintainer's domain, GitHub. Nothing is
  signed or uploaded here: sign-manifest.ps1 signs it with the PC offline.
.EXAMPLE
  .\make-manifest.ps1 -Bundle .tmp\release\0.1.11 -Version 0.1.11 -NotesFile notes.txt
#>
param(
    [Parameter(Mandatory)][string]$Bundle,
    [Parameter(Mandatory)][ValidatePattern('^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$')][string]$Version,
    [ValidatePattern('^[0-9]+$')][string]$GitLabProjectId = '89277322',
    [ValidatePattern('^[a-z0-9.-]+$')][string]$UpdateSite = 'recallupdates.lonebeast.net',
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
    "https://gitlab.com/api/v4/projects/$GitLabProjectId/packages/generic/recallupdates/$Version/RecallSetup.exe",
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
    # A plain string: Get-Content -Raw adds PSPath, PSDrive and more, which ConvertTo-Json writes out.
    notes       = if ($NotesFile) { [IO.File]::ReadAllText((Resolve-Path -LiteralPath $NotesFile).ProviderPath).Replace("`r`n", "`n") } else { '' }
    setup       = [ordered]@{ name = 'RecallSetup.exe'; size = $file.Length; sha256 = $sha; urls = $urls }
    revoke_keys = @()
}
$out = Join-Path $Bundle 'recall-update.unsigned.json'
# UTF-8 without a byte-order mark: the signature covers these exact bytes.
$json = $manifest | ConvertTo-Json -Depth 5
# The app reads at most 256 KiB (update_manifest.rs MANIFEST_LIMIT) and expects these field types.
$check = $json | ConvertFrom-Json
foreach ($field in 'version', 'revision', 'title', 'notes', 'published') {
    if ($check.$field -isnot [string]) { throw "The manifest's $field is not a string." }
}
if (@($check.setup.urls).Count -ne $urls.Count -or $check.setup.size -ne $file.Length -or $check.setup.sha256 -ne $sha) { throw 'The manifest does not hold the Setup it was made for.' }
$bytes = [Text.UTF8Encoding]::new($false).GetBytes($json)
if ($bytes.Length -gt 256KB) { throw "The manifest is $($bytes.Length) bytes; the app reads at most 262144." }
[IO.File]::WriteAllBytes($out, $bytes)
"Wrote $out (sequence $($manifest.sequence), Setup $($file.Length) bytes, SHA-256 $sha)"
"GitLab manifest: https://gitlab.com/api/v4/projects/$GitLabProjectId/packages/generic/recallupdates/current/recall-update.json"
