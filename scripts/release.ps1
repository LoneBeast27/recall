<#
.SYNOPSIS
Builds an urgent hotfix Recall Setup locally from a release tag, or checks the bundle the release
workflow (.github/workflows/release.yml) built.
.DESCRIPTION
Feature releases are built by the release workflow on a GitHub-hosted runner; take its bundle with
-ArtifactDirectory. A hotfix for a bug that affects users right now may be built on this PC from
the tagged public source; the tag push still runs the workflow as an independent re-check.
A developer tool: never shipped, never started by Recall or Setup. Publishing is a separate step.

Local build (no -ArtifactDirectory). Refused unless HEAD is exactly the v<Version> tag, the tag is
on origin/main (git fetch origin first) and no tracked file is modified. Then, in order:
  1. tests, as the release workflow runs them (skip with -SkipTests): the recorder workspace,
     the app (cargo test --locked), and the Setup unit tests (scripts/setup/build.ps1 -Test);
  2. release builds in their own target folders (-TargetDirectory), with RUSTFLAGS
     --remap-path-prefix for the user profile (~), CARGO_HOME (~/.cargo), RUSTUP_HOME (~/.rustup),
     the checkout (recall) and the target folder (target), appended to any RUSTFLAGS already set;
  3. Setup around the four Recall executables (online dependencies);
  4. a byte scan (scripts/dev/check-release-paths.ps1) of every executable, including the payload
     zip inside Setup: any user name, profile path, checkout parent folder or "@gmail.com" fails;
  5. a Defender scan (scripts/dev/scan-release.ps1) of Setup, the app, the FFmpeg archive (its
     SHA-256 must equal FFMPEG_SHA256 in release.yml), the FFmpeg runtime and the VC++ redist.
     Evidence goes to <output>-defender. Any detection fails.
Output: RecallSetup.exe, SHA256SUMS.txt, SOURCE-REVISION.txt and recall-app\ (the four Recall
executables, for inspection). SHA256SUMS.txt is written only after every check passed.

  -ArtifactDirectory <folder>  the extracted CI bundle (recall-release-<version>[-signed]):
                               RecallSetup.exe, SHA256SUMS.txt, SOURCE-REVISION.txt, recall-app\.
                               Its revision must be the v<Version> tag, on origin/main.

Signing gate: when code signing is configured (SIGNPATH_CONFIGURED=1 in the environment, or a
SIGNPATH_CONFIGURED file in the CI bundle), a Setup without a valid Authenticode signature is
refused, and a local build (always unsigned) is refused too: take the signed CI bundle instead.
.EXAMPLE
git switch --detach v0.1.5; powershell -NoProfile -File scripts\release.ps1 -Version 0.1.5 -FfmpegArchive D:\cache\ffmpeg-7.1.1-full_build-shared.zip -FfmpegDirectory D:\tools\ffmpeg-7.1.1-gyan-full-shared\bin -VcRedist D:\cache\vc_redist.x64.exe
.EXAMPLE
powershell -NoProfile -File scripts\release.ps1 -Version 0.1.5 -ArtifactDirectory D:\Downloads\recall-release-0.1.5
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$')][string]$Version,
    [string]$OutputDirectory,
    [string]$ArtifactDirectory,
    # Local build: the Gyan FFmpeg 7.1.1 full shared archive, its extracted bin folder, and vc_redist.x64.exe (Defender scan).
    [string]$FfmpegArchive,
    [string]$FfmpegDirectory,
    [string]$VcRedist,
    # Local build: where the test and release target folders go (default .tmp\release-target in this checkout).
    [string]$TargetDirectory,
    [switch]$SkipTests
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (!$OutputDirectory) { $OutputDirectory = Join-Path $repo ".tmp\release\$Version" }
$output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
$signingConfigured = $env:SIGNPATH_CONFIGURED -eq '1'
$tag = "v$Version"

# The release tag must name this revision and be on main (the updater only offers tags on main).
function Assert-TaggedOnMain([string]$Revision) {
    $tagged = git -C $repo rev-parse -q --verify "refs/tags/$tag^{commit}"
    if (!$tagged) { throw "There is no $tag tag in this checkout (git fetch --tags origin)." }
    if ($tagged -ne $Revision) { throw "$tag is $tagged, not $Revision." }
    $main = git -C $repo rev-parse -q --verify 'refs/remotes/origin/main^{commit}'
    if (!$main) { throw 'There is no origin/main in this checkout (git fetch origin).' }
    git -C $repo merge-base --is-ancestor $Revision $main
    if ($LASTEXITCODE -ne 0) { throw "$tag ($Revision) is not on origin/main ($main): merge it to main, then git fetch origin." }
    $url = git -C $repo remote get-url origin
    if ($url -notmatch 'github\.com[:/]LoneBeast27/recall(\.git)?/?$') { Write-Warning "origin is $url, not the public GitHub repository." }
}

function Assert-Signed([string]$Setup) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Setup
    if ($signature.Status -ne 'Valid') {
        throw "Code signing is configured, but $Setup is not validly signed ($($signature.Status)). Use the signed CI bundle."
    }
    if (!$signature.TimeStamperCertificate) { throw "Code signing is configured, but $Setup carries no timestamp." }
    "Signed by $($signature.SignerCertificate.Subject)"
}

if ($ArtifactDirectory) {
    $bundle = (Resolve-Path -LiteralPath $ArtifactDirectory).ProviderPath
    if (Test-Path -LiteralPath $output) { throw "Choose an unused release output directory: $output" }
    $setup = Join-Path $bundle 'RecallSetup.exe'
    foreach ($need in $setup, (Join-Path $bundle 'SHA256SUMS.txt'), (Join-Path $bundle 'SOURCE-REVISION.txt')) {
        if (!(Test-Path -LiteralPath $need -PathType Leaf)) { throw "The CI bundle lacks $need" }
    }
    if (Test-Path -LiteralPath (Join-Path $bundle 'SIGNPATH_CONFIGURED') -PathType Leaf) { $signingConfigured = $true }
    $revision = (Get-Content -LiteralPath (Join-Path $bundle 'SOURCE-REVISION.txt') -TotalCount 1).Trim()
    Assert-TaggedOnMain $revision
    $sha = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
    $listed = ((Get-Content -LiteralPath (Join-Path $bundle 'SHA256SUMS.txt') -TotalCount 1) -split '\s+')[0].ToLowerInvariant()
    if ($sha -ne $listed) { throw "RecallSetup.exe does not match SHA256SUMS.txt ($sha vs $listed)." }
    $fileVersion = (Get-Item -LiteralPath $setup).VersionInfo.FileVersion
    if ($fileVersion -ne "$Version.0") { throw "The CI bundle's Setup is version $fileVersion, not $Version." }
    if ($signingConfigured) { Assert-Signed $setup }
    else { Write-Warning 'Code signing is not configured: this Setup is unsigned.' }
    New-Item -ItemType Directory -Path $output | Out-Null
    Copy-Item -Path (Join-Path $bundle '*') -Destination $output -Recurse
    "Checked the CI bundle of Recall $Version from $revision ($sha)."
    return
}

if ($signingConfigured) {
    throw 'Code signing is configured: a local build is unsigned. Pass -ArtifactDirectory with the signed CI bundle.'
}

# Local build: the guard first, then the Defender inputs, so nothing is built from the wrong source.
$revision = git -C $repo rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or !$revision) { throw 'Cannot read the release source revision.' }
$dirty = git -C $repo status --porcelain --untracked-files=no
if ($LASTEXITCODE -ne 0 -or $dirty) { throw "Build the release from a clean checkout (tracked changes: $($dirty -join '; '))." }
Assert-TaggedOnMain $revision
if (Test-Path -LiteralPath $output) { throw "Choose an unused release output directory: $output" }
$defenderEvidence = "$output-defender"
if (Test-Path -LiteralPath $defenderEvidence) { throw "Choose an unused release output directory: $defenderEvidence exists" }

foreach ($need in @{ FfmpegArchive = $FfmpegArchive; FfmpegDirectory = $FfmpegDirectory; VcRedist = $VcRedist }.GetEnumerator()) {
    if (!$need.Value) { throw "A local build needs -$($need.Key) for the Defender scan (Get-Help scripts\release.ps1 -Examples)." }
}
$FfmpegArchive = (Resolve-Path -LiteralPath $FfmpegArchive).ProviderPath
$FfmpegDirectory = (Resolve-Path -LiteralPath $FfmpegDirectory).ProviderPath
$VcRedist = (Resolve-Path -LiteralPath $VcRedist).ProviderPath
$workflow = Get-Content -Raw -LiteralPath (Join-Path $repo '.github\workflows\release.yml')
if ($workflow -notmatch 'FFMPEG_SHA256:\s*([0-9A-Fa-f]{64})') { throw 'Cannot read FFMPEG_SHA256 from release.yml.' }
$ffmpegSha = (Get-FileHash -LiteralPath $FfmpegArchive -Algorithm SHA256).Hash
if ($ffmpegSha -ne $Matches[1].ToUpperInvariant()) { throw "$FfmpegArchive is $ffmpegSha, not the pinned FFmpeg archive $($Matches[1])." }

$common = git -C $repo rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { throw 'Cannot locate shared Recall build tools.' }
$tools = if ($env:RECALL_TOOLS_ROOT) { (Resolve-Path -LiteralPath $env:RECALL_TOOLS_ROOT).ProviderPath } else { Split-Path ([IO.Path]::GetFullPath($common)) -Parent }
if (!$TargetDirectory) { $TargetDirectory = Join-Path $repo '.tmp\release-target' }
$target = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($TargetDirectory)
$nice = Join-Path $env:USERPROFILE '.claude\tools\nice-build.ps1'
function Invoke-Build { if (Test-Path -LiteralPath $nice) { & powershell -NoProfile -File $nice @args } else { & $args[0] @($args | Select-Object -Skip 1) } }

# Paths rustc writes into panic messages and file!() become ~, ~/.cargo, ~/.rustup, recall, target.
# rustc lets the last matching prefix win, so the longer (more specific) prefixes go last.
function Get-RemapFlags {
    $home_ = [IO.Path]::GetFullPath($env:USERPROFILE)
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $home_ '.cargo' }
    $rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $home_ '.rustup' }
    $maps = [ordered]@{}
    foreach ($pair in @($home_, '~'), @($cargoHome, '~/.cargo'), @($rustupHome, '~/.rustup'), @($tools, 'recall'), @($repo, 'recall'), @($target, 'target')) {
        $from = [IO.Path]::GetFullPath($pair[0]).TrimEnd('\')
        if ($from -match '\s') { throw "Cannot remap a path with spaces through RUSTFLAGS: $from" }
        $maps[$from] = $pair[1]
    }
    $maps.Keys | Sort-Object Length | ForEach-Object { "--remap-path-prefix=$_=$($maps[$_])" }
}

$saved = @{}
foreach ($name in 'RECALL_TOOLS_ROOT', 'RECALL_TARGET_ROOT', 'RECALL_VERSION', 'RECALL_SOURCE_REVISION', 'RUSTFLAGS', 'CARGO_TARGET_DIR', 'FFMPEG_DIR', 'LIBCLANG_PATH', 'PATH') {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
Push-Location $repo
try {
    if ($env:CARGO_ENCODED_RUSTFLAGS) { throw 'CARGO_ENCODED_RUSTFLAGS would replace the path remapping: unset it.' }
    $env:RECALL_TOOLS_ROOT = $tools
    $env:RECALL_TARGET_ROOT = $target
    # Keeps the build a release build (cfg recall_release: no CARGO_MANIFEST_DIR in recall-native).
    $env:RECALL_VERSION = $Version
    # The exact build (the tag's commit): the app and Setup compare it to SOURCE-REVISION.txt.
    $env:RECALL_SOURCE_REVISION = $revision

    if ($SkipTests) { Write-Warning 'Tests skipped (-SkipTests).' }
    else {
        # Same calls as release.yml; locally every test runs (no runner skips).
        $ffmpegDev = Join-Path $tools 'tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared'
        $env:FFMPEG_DIR = $ffmpegDev
        if (!$env:LIBCLANG_PATH) { $env:LIBCLANG_PATH = Join-Path $tools '.tmp\nvenc-vfr\clang18\clang\native' }
        $env:PATH = "$(Join-Path $ffmpegDev 'bin');$env:PATH"
        $env:CARGO_TARGET_DIR = Join-Path $target 'test-dev'
        Invoke-Build cargo test --locked -p recall -p recall-hdr -p recall-nvidia -p recall-overlay
        if ($LASTEXITCODE -ne 0) { throw "recorder tests failed ($LASTEXITCODE)" }
        $env:CARGO_TARGET_DIR = Join-Path $target 'test-gpui'
        Invoke-Build cargo test --locked --manifest-path recall-native\Cargo.toml
        if ($LASTEXITCODE -ne 0) { throw "app tests failed ($LASTEXITCODE)" }
        Remove-Item Env:CARGO_TARGET_DIR
        & (Join-Path $PSScriptRoot 'setup\build.ps1') -Test -OutputPath (Join-Path $target 'setup-tests\RecallSetupTests.exe')
        "Tests passed."
    }

    $remap = @(Get-RemapFlags)
    $env:RUSTFLAGS = (@($saved['RUSTFLAGS']) + $remap | Where-Object { $_ }) -join ' '
    "RUSTFLAGS: $env:RUSTFLAGS"
    $payload = Join-Path $output 'recall-app'
    & powershell -NoProfile -File (Join-Path $PSScriptRoot 'install.ps1') -StageDir $payload
    if ($LASTEXITCODE -ne 0) { throw "Recall build failed ($LASTEXITCODE)." }
    $parents = @((Split-Path $repo -Parent), (Split-Path $tools -Parent)) | Sort-Object -Unique
    $scan = Join-Path $PSScriptRoot 'dev\check-release-paths.ps1'
    & $scan -Path $payload -ParentPath $parents
    if ($LASTEXITCODE -ne 0) { throw 'The Recall executables carry local paths (see LOCAL-PATH HIT above).' }

    $exe = Join-Path $output 'RecallSetup.exe'
    & (Join-Path $PSScriptRoot 'setup\build.ps1') -PayloadDirectory $payload -OnlineDependencies -OutputPath $exe | Out-Null
    & $scan -Path $exe, $payload -ParentPath $parents
    if ($LASTEXITCODE -ne 0) { throw 'Setup carries local paths (see LOCAL-PATH HIT above).' }

    & (Join-Path $PSScriptRoot 'dev\scan-release.ps1') -Setup $exe -AppDirectory $payload -FfmpegArchive $FfmpegArchive `
        -FfmpegDirectory $FfmpegDirectory -VcRedist $VcRedist -EvidenceDirectory $defenderEvidence

    $sha = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath (Join-Path $output 'SOURCE-REVISION.txt') -Value $revision -Encoding ascii
    Set-Content -LiteralPath (Join-Path $output 'SHA256SUMS.txt') -Value "$sha  RecallSetup.exe" -Encoding ascii
    "Built Recall $Version from $revision ($sha) in $output; Defender evidence in $defenderEvidence."
    'Release notes line:'
    "Hotfix built locally from tag $tag (source revision $revision); the GitHub workflow re-checks the same tag: https://github.com/LoneBeast27/recall/actions/runs/<run id>"
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
    Pop-Location
}
