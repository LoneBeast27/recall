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
  .\publish.ps1 -Bundle .tmp\release\0.1.11 -Skip github
#>
param(
    [Parameter(Mandatory)][string]$Bundle,
    [ValidatePattern('^[0-9]+$')][string]$GitLabProjectId = '89277322',
    [string]$UpdateSite = 'recallupdates.lonebeast.net',
    [string]$PagesProject = 'recallupdates',
    # The local copy of the Pages site (Pages replaces the whole site on each deploy).
    [string]$SiteFolder = (Join-Path $Bundle '..\pages-site'),
    [ValidateSet('gitlab', 'pages', 'github')][string[]]$Only = @('gitlab', 'pages', 'github'),
    [ValidateSet('gitlab', 'pages', 'github')][string[]]$Skip = @()
)
$ErrorActionPreference = 'Stop'
$hosts = $Only | Where-Object { $Skip -notcontains $_ }
$manifest = Join-Path $Bundle 'recall-update.json'
$signature = "$manifest.minisig"
$setup = Join-Path $Bundle 'RecallSetup.exe'
foreach ($f in $manifest, $signature, $setup) { if (!(Test-Path -LiteralPath $f)) { throw "Missing $f (sign first)." } }
$m = Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json
$version = $m.version
$results = [ordered]@{}

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
        $versionDir = Join-Path $SiteFolder "v$version"
        New-Item -ItemType Directory -Force -Path $versionDir | Out-Null
        Copy-Item -LiteralPath $setup -Destination $versionDir -Force
        Copy-Item -LiteralPath $manifest, $signature -Destination $SiteFolder -Force
        # Keep this version and the one before it; Pages serves only what the last deploy holds.
        Get-ChildItem -LiteralPath $SiteFolder -Directory | Where-Object Name -like 'v*' |
            Sort-Object { [version]$_.Name.Substring(1) } -Descending | Select-Object -Skip 2 |
            ForEach-Object { Remove-Item -LiteralPath $_.FullName -Recurse -Force }
        # A pinned, already installed wrangler (never a fresh npx download with the token set):
        # the version is recorded once in wrangler-version.txt after `npm install -g wrangler@<v>`.
        $pinned = (Get-Content -LiteralPath (Join-Path $PSScriptRoot 'wrangler-version.txt') -TotalCount 1).Trim()
        $installed = ((& wrangler --version) | Select-Object -First 1) -replace '[^0-9.]', ''
        if ($pinned -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$' -or $installed -ne $pinned) { throw "wrangler $installed is installed; $pinned is pinned in wrangler-version.txt." }
        & wrangler pages deploy $SiteFolder --project-name $PagesProject --branch main
        if ($LASTEXITCODE -ne 0) { throw 'wrangler pages deploy failed.' }
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
