<#
Builds a versioned, online-dependency Recall Setup from the public main commit, or takes the
bundle the release workflow (.github/workflows/release.yml) built and checks it.
The four Recall executables are kept in a separate staging folder for inspection.
Outputs Setup, its SHA-256, and its exact source revision. Publishing is a separate step.
A developer tool: never shipped, never started by Recall or Setup.

  -ArtifactDirectory <folder>  the extracted CI bundle (recall-release-<version>[-signed]):
                               RecallSetup.exe, SHA256SUMS.txt, SOURCE-REVISION.txt, recall-app\.

Signing gate: when code signing is configured (SIGNPATH_CONFIGURED=1 in the environment, or a
SIGNPATH_CONFIGURED file in the CI bundle), a Setup without a valid Authenticode signature is
refused, and a local build (always unsigned) is refused too: take the signed CI bundle instead.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$')][string]$Version,
    [string]$OutputDirectory,
    [string]$ArtifactDirectory
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (!$OutputDirectory) { $OutputDirectory = Join-Path $repo ".tmp\release\$Version" }
$output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
$signingConfigured = $env:SIGNPATH_CONFIGURED -eq '1'

function Get-PublicMain {
    $remote = git ls-remote 'https://github.com/LoneBeast27/recall.git' 'refs/heads/main'
    if ($LASTEXITCODE -ne 0 -or !$remote) { throw 'Cannot read the public GitHub main commit.' }
    ($remote -split '\s+')[0]
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
    if ($revision -ne (Get-PublicMain)) { throw "The CI bundle was built from $revision, not the current public GitHub main commit." }
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

Push-Location $repo
try {
    $revision = git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read the release source revision.' }
    $dirty = git status --porcelain --untracked-files=no
    if ($LASTEXITCODE -ne 0 -or $dirty) { throw 'Build the release from a clean checkout.' }
    if ((Get-PublicMain) -ne $revision) {
        throw 'Build the release from the current public GitHub main commit.'
    }
    if (Test-Path -LiteralPath $output) { throw "Choose an unused release output directory: $output" }

    $common = git rev-parse --path-format=absolute --git-common-dir
    if ($LASTEXITCODE -ne 0) { throw 'Cannot locate shared Recall build tools.' }
    $tools = Split-Path $common -Parent
    $previousTools = $env:RECALL_TOOLS_ROOT
    $previousVersion = $env:RECALL_VERSION
    try {
        if (!$previousTools) { $env:RECALL_TOOLS_ROOT = $tools }
        $env:RECALL_VERSION = $Version
        $payload = Join-Path $output 'recall-app'
        & powershell -NoProfile -File (Join-Path $PSScriptRoot 'install.ps1') -StageDir $payload
        if ($LASTEXITCODE -ne 0) { throw "Recall build failed ($LASTEXITCODE)." }
        $exe = Join-Path $output 'RecallSetup.exe'
        & (Join-Path $PSScriptRoot 'setup\build.ps1') -PayloadDirectory $payload -OnlineDependencies -OutputPath $exe
        $sha = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
        Set-Content -LiteralPath (Join-Path $output 'SHA256SUMS.txt') -Value "$sha  RecallSetup.exe" -Encoding ascii
        Set-Content -LiteralPath (Join-Path $output 'SOURCE-REVISION.txt') -Value $revision -Encoding ascii
        "Built Recall $Version from $revision."
    } finally {
        $env:RECALL_TOOLS_ROOT = $previousTools
        $env:RECALL_VERSION = $previousVersion
    }
} finally {
    Pop-Location
}
