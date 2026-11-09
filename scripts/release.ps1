<#
Builds a versioned, online-dependency Recall Setup from the public main commit.
The four Recall executables are kept in a separate staging folder for inspection.
Outputs Setup, its SHA-256, and its exact source revision. Publishing is a separate step.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$')][string]$Version,
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (!$OutputDirectory) { $OutputDirectory = Join-Path $repo ".tmp\release\$Version" }
$output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
Push-Location $repo
try {
    $revision = git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read the release source revision.' }
    $dirty = git status --porcelain --untracked-files=no
    if ($LASTEXITCODE -ne 0 -or $dirty) { throw 'Build the release from a clean checkout.' }
    $remote = git ls-remote 'https://github.com/LoneBeast27/recall.git' 'refs/heads/main'
    if ($LASTEXITCODE -ne 0 -or ($remote -split '\s+')[0] -ne $revision) {
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
