<#
.SYNOPSIS
  Publishes a signed release to the update tiers: GitLab, the update site on Cloudflare Pages,
  and the GitHub release (the third copy). Run it from the release login, never the daily one.
.DESCRIPTION
  Uploads the same signed files everywhere; each host is its own step, and one failing does not
  stop the others. Afterwards every public manifest is downloaded again and compared byte for
  byte with the signed one. Re-run a failed host later with -Only <host>; nothing is re-signed.
  Tokens come from the release login's environment and are never printed:
    RECALL_GITLAB_DEPLOY_TOKEN  deploy token named recallupdates, only write_package_registry, for the one project
                                (free on GitLab.com; project access tokens need a paid plan there)
    CLOUDFLARE_API_TOKEN   Pages edit token for the one account; CLOUDFLARE_ACCOUNT_ID beside it
  GitHub uses the gh CLI; on the release login it should hold no token, so -Skip github there
  and add the GitHub copy from the daily login with -Only github.
.EXAMPLE
  .\publish.ps1 -Bundle .tmp\release\0.1.11 -PreviousBundle .tmp\release\0.1.10 -Skip github
#>
param(
    [Parameter(Mandatory)][string]$Bundle,
    [ValidatePattern('^[0-9]+$')][string]$GitLabProjectId = '89277322',
    [string]$UpdateSite = 'recallupdates.lonebeast.net',
    [string]$PagesProject = 'recallupdates',
    # Optional: the previous release's verified bundle (RecallSetup.exe + SHA256SUMS.txt), so the
    # update site keeps serving that Setup too. Pages replaces the whole site on each deploy.
    [string]$PreviousBundle,
    [ValidateSet('gitlab', 'pages', 'github')][string[]]$Only = @('gitlab', 'pages', 'github'),
    [ValidateSet('gitlab', 'pages', 'github')][string[]]$Skip = @()
)
$ErrorActionPreference = 'Stop'
$hosts = @($Only | Where-Object { $Skip -notcontains $_ })
$manifest = Join-Path $Bundle 'recall-update.json'
$signature = "$manifest.minisig"
$setup = Join-Path $Bundle 'RecallSetup.exe'
foreach ($f in $manifest, $signature, $setup) { if (!(Test-Path -LiteralPath $f)) { throw "Missing $f (sign first)." } }
$m = Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json
$version = $m.version
if ($version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw "The manifest names an invalid version: $version" }
$results = [ordered]@{}

# A verified bundle's Setup: SHA256SUMS.txt must name it, and its file version gives the version.
function Get-VerifiedSetup([string]$Folder) {
    $exe = Join-Path $Folder 'RecallSetup.exe'
    $sums = Join-Path $Folder 'SHA256SUMS.txt'
    foreach ($f in $exe, $sums) { if (!(Test-Path -LiteralPath $f -PathType Leaf)) { throw "Missing $f" } }
    $listed = ((Get-Content -LiteralPath $sums -TotalCount 1) -split '\s+')[0].ToLowerInvariant()
    $sha = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($sha -ne $listed) { throw "$exe does not match $sums ($sha vs $listed)." }
    $v = (Get-Item -LiteralPath $exe).VersionInfo.FileVersion -replace '\.0$', ''
    if ($v -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw "$exe has no release version ($v)." }
    [pscustomobject]@{ Path = $exe; Version = $v }
}

# Wrangler runs with metrics off and IPv4 first (an IP-filtered token may not list this PC's IPv6).
function Invoke-Wrangler {
    $saved = @{ WRANGLER_SEND_METRICS = $env:WRANGLER_SEND_METRICS; NODE_OPTIONS = $env:NODE_OPTIONS }
    $env:WRANGLER_SEND_METRICS = 'false'; $env:NODE_OPTIONS = '--dns-result-order=ipv4first'
    try { & wrangler @args } finally { foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k], 'Process') } }
}

# Everything is checked before the first upload: what goes where, and the pinned wrangler.
$previous = $null
if ($hosts -contains 'pages') {
    # A pinned, already installed wrangler (never a fresh npx download with the token set):
    # the version is recorded once in wrangler-version.txt after `npm install -g wrangler@<v>`.
    $pinned = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'wrangler-version.txt') -TotalCount 1).Trim()
    $installed = ((Invoke-Wrangler --version) | Select-Object -First 1) -replace '[^0-9.]', ''
    if ($pinned -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$' -or $installed -ne $pinned) { throw "wrangler $installed is installed; $pinned is pinned in wrangler-version.txt." }
    if ($PreviousBundle) {
        $previous = Get-VerifiedSetup ((Resolve-Path -LiteralPath $PreviousBundle).ProviderPath)
        if ($previous.Version -eq $version) { throw "-PreviousBundle is the same version ($version)." }
    }
}
"Publishing Recall $version to: $($hosts -join ', ')"
if ($hosts -contains 'gitlab') { "  gitlab: project $GitLabProjectId, generic package recallupdates ($version/RecallSetup.exe, current/recall-update.json + .minisig)" }
if ($hosts -contains 'pages') { "  pages:  project $PagesProject at https://$UpdateSite (recall-update.json + .minisig, v$version/RecallSetup.exe$(if ($previous) { ", v$($previous.Version)/RecallSetup.exe" }))" }
if ($hosts -contains 'github') { "  github: release v$version (recall-update.json + .minisig)" }

function Step([string]$name, [scriptblock]$body) {
    try { & $body; $results[$name] = 'published' } catch { $results[$name] = "failed: $($_.Exception.Message)" }
}

if ($hosts -contains 'gitlab') {
    Step 'gitlab' {
        $token = $env:RECALL_GITLAB_DEPLOY_TOKEN
        if (!$token) { throw 'RECALL_GITLAB_DEPLOY_TOKEN is not set.' }
        $packages = "https://gitlab.com/api/v4/projects/$GitLabProjectId/packages/generic"
        $headers = @{ 'Deploy-Token' = $token }
        # Setup under its version; the manifest under a fixed name, where a download returns the
        # newest upload. No commit to main (protected) and no release object is needed.
        Invoke-RestMethod -Method Put -Uri "$packages/recallupdates/$version/RecallSetup.exe" -Headers $headers -InFile $setup | Out-Null
        foreach ($f in $manifest, $signature) {
            Invoke-RestMethod -Method Put -Uri "$packages/recallupdates/current/$(Split-Path $f -Leaf)" -Headers $headers -InFile $f | Out-Null
        }
    }
}
if ($hosts -contains 'pages') {
    Step 'pages' {
        if (!$env:CLOUDFLARE_API_TOKEN -or !$env:CLOUDFLARE_ACCOUNT_ID) { throw 'CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID must be set.' }
        # A fresh site folder per run holding exactly the signed files, never a reused folder.
        $site = Join-Path ([IO.Path]::GetTempPath()) "recall-pages-$([guid]::NewGuid().ToString('N'))"
        try {
            New-Item -ItemType Directory -Path (Join-Path $site "v$version") | Out-Null
            Copy-Item -LiteralPath $setup -Destination (Join-Path $site "v$version")
            Copy-Item -LiteralPath $manifest, $signature -Destination $site
            $siteFiles = @('recall-update.json', 'recall-update.json.minisig', "v$version/RecallSetup.exe")
            if ($previous) {
                New-Item -ItemType Directory -Path (Join-Path $site "v$($previous.Version)") | Out-Null
                Copy-Item -LiteralPath $previous.Path -Destination (Join-Path $site "v$($previous.Version)")
                $siteFiles += "v$($previous.Version)/RecallSetup.exe"
            }
            $found = @(Get-ChildItem -LiteralPath $site -Recurse -File -Force | ForEach-Object { $_.FullName.Substring($site.Length + 1).Replace('\', '/') })
            $extra = @($found | Where-Object { $siteFiles -notcontains $_ }) + @($siteFiles | Where-Object { $found -notcontains $_ })
            if ($extra) { throw "The site folder is not exactly the signed files: $($extra -join ', ')" }
            Invoke-Wrangler pages deploy $site --project-name $PagesProject --branch main
            if ($LASTEXITCODE -ne 0) { throw 'wrangler pages deploy failed.' }
        } finally {
            if (Test-Path -LiteralPath $site) { Remove-Item -LiteralPath $site -Recurse -Force }
        }
    }
}
if ($hosts -contains 'github') {
    Step 'github' {
        gh release upload "v$version" $manifest $signature --clobber --repo LoneBeast27/recall
        if ($LASTEXITCODE -ne 0) { throw 'gh release upload failed.' }
    }
}

# Read every public copy back: the bytes must be exactly the signed file.
$expected = [IO.File]::ReadAllBytes($manifest)
$public = @{
    gitlab = "https://gitlab.com/api/v4/projects/$GitLabProjectId/packages/generic/recallupdates/current/recall-update.json"
    pages  = "https://$UpdateSite/recall-update.json"
    github = 'https://github.com/LoneBeast27/recall/releases/latest/download/recall-update.json'
}
foreach ($name in $hosts) {
    if ($results[$name] -ne 'published') { continue }
    try {
        $got = (Invoke-WebRequest -Uri $public[$name] -UseBasicParsing).Content
        $bytes = if ($got -is [byte[]]) { $got } else { [Text.Encoding]::UTF8.GetBytes($got) }
        if ([Convert]::ToBase64String($bytes) -ne [Convert]::ToBase64String($expected)) { $results[$name] = 'failed: the public copy differs from the signed file' }
    } catch { $results[$name] = "failed: read-back: $($_.Exception.Message)" }
}
$results.GetEnumerator() | ForEach-Object { '{0,-7} {1}' -f $_.Key, $_.Value }
if ($results.Values | Where-Object { $_ -ne 'published' }) { exit 1 }
