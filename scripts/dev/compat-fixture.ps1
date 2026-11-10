# Makes compat/settings/<tag>/: the settings files a release itself writes, for the compatibility
# tests (CONTRIBUTING.md, "Settings compatibility"). Run it through the build gate after a release:
#   powershell -NoProfile -File L:\General\.tmp\recall-wave-a\gated.ps1 none powershell -NoProfile -File scripts\dev\compat-fixture.ps1 -Tag v0.1.2
#
# config.toml: the release's own loader reads -InputToml (default: the newest fixture's
# config.toml; give every setting the release added a non-default example value first) and its own
# save writes it. The run stops if that release changed (repaired) any value of the input.
# default.toml: the release's defaults, with the video folder replaced by an example path.
# The JSON files are copied from the newest fixture (edit them for new keys), then the release's own
# loaders read and save them unchanged (compat-rollback.ps1 -ReadDir) before they count.
# Fixtures hold example values only: no user names, real paths, device names or monitor ids.
param(
    [Parameter(Mandatory = $true)][string]$Tag,
    [string]$InputToml = '',
    [string]$TargetDir = ''
)
$ErrorActionPreference = 'Continue'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $TargetDir) { $TargetDir = Join-Path $repo '.tmp\compat-rollback-target' }
$settings = Join-Path $repo 'compat\settings'
$dest = Join-Path $settings $Tag
$newest = Get-ChildItem -LiteralPath $settings -Directory | Where-Object { $_.Name -match '^v\d+\.\d+\.\d+$' -and $_.Name -ne $Tag } |
    Sort-Object { [version]($_.Name.Substring(1)) } | Select-Object -Last 1
if (-not $InputToml) { $InputToml = Join-Path $newest.FullName 'config.toml' }

$work = Join-Path ([IO.Path]::GetTempPath()) ("recall-compat-fixture-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$old = Join-Path $work 'tree'
$out = Join-Path $work 'out'
New-Item -ItemType Directory -Force -Path $out | Out-Null
try {
    & git -C $repo worktree add --detach $old $Tag 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw "git worktree add $Tag failed" }
    $writer = (Join-Path $repo 'compat\tools\fixture_writer.rs').Replace('\', '/')
    [IO.File]::AppendAllText((Join-Path $old 'recall-hdr\src\config.rs'), "`n#[cfg(test)]`n#[path = `"$writer`"]`nmod compat_fixture;`n")
    Copy-Item -LiteralPath $InputToml -Destination (Join-Path $out 'input.toml')
    $env:COMPAT_DIR = $out
    $env:CARGO_TARGET_DIR = Join-Path $TargetDir 'old-hdr'
    & cargo test --locked --manifest-path "$old\Cargo.toml" -p recall-hdr compat_fixture::compat_fixture_writer 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw "the fixture writer failed at $Tag" }
    $report = Get-Content -Raw -LiteralPath (Join-Path $out 'report.txt')
    Write-Output $report
    if ($report -notmatch 'strict_equals_load=true' -or $report -notmatch 'repairs=\[\]') { throw "$Tag did not read the input as written; fix input.toml" }

    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Copy-Item -LiteralPath (Join-Path $out 'config.toml') -Destination (Join-Path $dest 'config.toml') -Force
    $default = Get-Content -Raw -LiteralPath (Join-Path $out 'default.toml')
    $default = [regex]::Replace($default, "(?m)^output_dir = .*$", "output_dir = 'C:\Users\example\Videos\Recall'")
    [IO.File]::WriteAllText((Join-Path $dest 'default.toml'), $default)
    foreach ($json in Get-ChildItem -LiteralPath $newest.FullName -Filter *.json) {
        $to = Join-Path $dest $json.Name
        if (-not (Test-Path -LiteralPath $to)) { Copy-Item -LiteralPath $json.FullName -Destination $to }
    }
} finally {
    Remove-Item Env:COMPAT_DIR -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $old) { & git -C $repo worktree remove --force $old 2>&1 | ForEach-Object { "$_" } }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

# The release itself must read and save the app files unchanged.
& powershell -NoProfile -File (Join-Path $PSScriptRoot 'compat-rollback.ps1') -Previous $Tag -ReadDir $dest -TargetDir $TargetDir
if ($LASTEXITCODE -ne 0) { throw "$Tag changed its own settings files" }
Write-Output "Wrote $dest. Check it holds example values only, then commit it."
