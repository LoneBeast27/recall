# Rollback compatibility check: the previous release reads the settings files this build writes.
#
# 1. This build saves the newest fixture of each settings file (compat/settings/v*/) into a
#    temp folder (the compatibility tests' write_this_builds_file*, RECALL_COMPAT_WRITE_DIR).
# 2. The previous release is checked out into a temp worktree, and this tree's compatibility
#    harnesses are mounted into it (one #[path] line per harness, never committed there).
# 3. The previous release's own loaders read those files and save them again: every value it
#    knows must arrive unchanged, every key it does not know must survive its save.
#
# Runs in the release workflow; locally run it through the build gate (it builds both releases):
#   powershell -NoProfile -File L:\General\.tmp\recall-wave-a\gated.ps1 none powershell -NoProfile -File scripts\dev\compat-rollback.ps1
# Options: -Previous v0.1.1 (default: the newest v* tag below HEAD), -ReadDir <folder> (read the
# files there instead of writing them first, e.g. compat\settings\v0.1.1 to prove that release
# reads its own fixture), -TargetDir <folder> (cargo targets; default .tmp\compat-rollback-target),
# -HdrTarget / -AppTarget <folder> (this build's targets, to reuse an earlier test build's).
param(
    [string]$Previous = '',
    [string]$ReadDir = '',
    [string]$TargetDir = '',
    [string]$HdrTarget = '',
    [string]$AppTarget = ''
)
# Continue: Windows PowerShell turns a native tool's stderr (cargo's progress) into errors under Stop.
$ErrorActionPreference = 'Continue'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $TargetDir) { $TargetDir = Join-Path $repo '.tmp\compat-rollback-target' }
if (-not $HdrTarget) { $HdrTarget = Join-Path $TargetDir 'new-hdr' }
if (-not $AppTarget) { $AppTarget = Join-Path $TargetDir 'new-app' }

function Invoke-Cargo([string]$what, [string[]]$cargoArgs) {
    Write-Output "== $what"
    & cargo @cargoArgs 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw "$what failed ($LASTEXITCODE)" }
}

if (-not $Previous) {
    $here = (& git -C $repo describe --tags --exact-match HEAD 2>$null)
    $Previous = & git -C $repo tag --list 'v*' --sort=-v:refname --merged HEAD | Where-Object { $_ -ne $here } | Select-Object -First 1
    if (-not $Previous) { throw 'no earlier release tag (v*) below HEAD' }
}
Write-Output "Previous release: $Previous"

$work = Join-Path ([IO.Path]::GetTempPath()) ("recall-compat-rollback-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$files = Join-Path $work 'files'
$old = Join-Path $work 'old'
New-Item -ItemType Directory -Force -Path $files | Out-Null

try {
    if ($ReadDir) {
        $files = (Resolve-Path $ReadDir).Path
    } else {
        $env:RECALL_COMPAT_WRITE_DIR = $files
        $env:CARGO_TARGET_DIR = $HdrTarget
        Invoke-Cargo 'this build writes config.toml' @('test', '--locked', '--manifest-path', "$repo\Cargo.toml", '-p', 'recall-hdr', 'compatibility::write_this_builds_file')
        $env:CARGO_TARGET_DIR = $AppTarget
        Invoke-Cargo 'this build writes the app files' @('test', '--locked', '--manifest-path', "$repo\recall-native\Cargo.toml", 'compatibility::write_this_builds_file')
        Remove-Item Env:RECALL_COMPAT_WRITE_DIR
    }
    Write-Output "Files the previous release reads:"
    Get-ChildItem -LiteralPath $files -File | ForEach-Object { "  $($_.Name)" }

    & git -C $repo worktree add --detach $old $Previous 2>&1 | ForEach-Object { "$_" }
    if ($LASTEXITCODE -ne 0) { throw "git worktree add $Previous failed" }

    # Mount this tree's harnesses (absolute paths, forward slashes) as `compat_rollback`.
    $mounts = @(
        @{ Into = 'recall-hdr\src\config.rs'; Harness = 'recall-hdr\src\compatibility_tests.rs' },
        @{ Into = 'recall-native\src\services\library.rs'; Harness = 'recall-native\src\services\library\compatibility_tests.rs' },
        @{ Into = 'recall-native\src\services\update.rs'; Harness = 'recall-native\src\services\update_compatibility_tests.rs'; Needs = 'recall-native\src\services\update_prefs.rs' }
    )
    foreach ($m in $mounts) {
        if ($m.Needs -and -not (Test-Path -LiteralPath (Join-Path $old $m.Needs))) { Write-Output "  $Previous has no $($m.Needs): skipped"; continue }
        $path = (Join-Path $repo $m.Harness).Replace('\', '/')
        $line = "`n#[cfg(test)]`n#[path = `"$path`"]`nmod compat_rollback;`n"
        [IO.File]::AppendAllText((Join-Path $old $m.Into), $line)
    }

    $env:RECALL_COMPAT_ROLLBACK_DIR = $files
    $env:RECALL_COMPAT_ROOT = Join-Path $repo 'compat'
    $env:CARGO_TARGET_DIR = Join-Path $TargetDir 'old-hdr'
    Invoke-Cargo "$Previous reads config.toml" @('test', '--locked', '--manifest-path', "$old\Cargo.toml", '-p', 'recall-hdr', 'compat_rollback::rollback_keeps')
    $env:CARGO_TARGET_DIR = Join-Path $TargetDir 'old-app'
    Invoke-Cargo "$Previous reads the app files" @('test', '--locked', '--manifest-path', "$old\recall-native\Cargo.toml", 'compat_rollback::rollback_keeps')
    Write-Output "Rollback to $Previous keeps every setting."
} finally {
    Remove-Item Env:RECALL_COMPAT_ROLLBACK_DIR, Env:RECALL_COMPAT_ROOT, Env:RECALL_COMPAT_WRITE_DIR -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $old) { & git -C $repo worktree remove --force $old 2>&1 | ForEach-Object { "$_" } }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
