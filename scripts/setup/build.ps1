[CmdletBinding(DefaultParameterSetName='Preview')]
param(
    [Parameter(ParameterSetName='Preview')][switch]$Preview,
    [Parameter(Mandatory=$true, ParameterSetName='Release')][string]$PayloadDirectory,
    [string]$OutputPath
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$assets = Join-Path $repo 'recall-native\assets'
$compiler = Join-Path $env:windir 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
$nice = Join-Path $env:USERPROFILE '.claude\tools\nice-build.ps1'
$isPreview = $PSCmdlet.ParameterSetName -eq 'Preview'
if (!$OutputPath) {
    $name = if ($isPreview) { 'RecallSetupPreview.exe' } else { 'RecallSetup.exe' }
    $OutputPath = Join-Path $repo "dist\setup\$name"
}
$OutputPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
New-Item -ItemType Directory -Force -Path (Split-Path $OutputPath -Parent) | Out-Null
if (!(Test-Path -LiteralPath $nice -PathType Leaf)) { throw "Build wrapper not found at $nice" }

function Copy-PackageFile([string]$Source, [string]$Destination) {
    if (!(Test-Path -LiteralPath $Source -PathType Leaf)) { throw "Missing setup input: $Source" }
    New-Item -ItemType Directory -Force -Path (Split-Path $Destination -Parent) | Out-Null
    Copy-Item -LiteralPath $Source -Destination $Destination
}

$arguments = @(
    '/nologo', '/warnaserror', '/target:winexe', '/platform:x64', '/optimize+', '/utf8output',
    '/reference:System.Drawing.dll', '/reference:System.Windows.Forms.dll',
    '/reference:System.IO.Compression.dll', '/reference:System.IO.Compression.FileSystem.dll',
    "/out:$OutputPath", "/win32icon:$assets\recall.ico",
    "/resource:$assets\fonts\bricolage-grotesque-700.ttf,Recall.Display.ttf",
    "/resource:$assets\fonts\geist-400.ttf,Recall.Body.ttf",
    "/resource:$assets\fonts\geist-600.ttf,Recall.Strong.ttf",
    "$PSScriptRoot\Program.cs", "$PSScriptRoot\SetupWindow.cs", "$PSScriptRoot\SetupWindow.Guided.cs", "$PSScriptRoot\SetupPackage.cs"
)
$work = $null
try {
    if ($isPreview) {
        $arguments += '/define:PREVIEW'
    } else {
        $source = (Resolve-Path -LiteralPath $PayloadDirectory).ProviderPath
        $workspace = Join-Path $repo '.tmp\setup'
        New-Item -ItemType Directory -Force -Path $workspace | Out-Null
        $work = Join-Path $workspace ([Guid]::NewGuid().ToString('N'))
        $package = Join-Path $work 'payload'
        $installerScripts = 'install.ps1', 'install-swap.ps1', 'install-quit-wait.ps1'
        foreach ($name in $installerScripts) {
            Copy-PackageFile (Join-Path $repo "scripts\$name") (Join-Path $package "scripts\$name")
        }
        Copy-PackageFile (Join-Path $source 'recall-native.exe') (Join-Path $package 'target-gpui\release\recall-native.exe')
        foreach ($name in 'recall.exe','recall-hdr.exe','recall-overlay.exe') {
            Copy-PackageFile (Join-Path $source $name) (Join-Path $package "target-dev\release\$name")
        }
        $runtime = 'ffmpeg.exe','ffprobe.exe','avcodec-61.dll','avdevice-61.dll','avfilter-10.dll',
            'avformat-61.dll','avutil-59.dll','postproc-58.dll','swresample-5.dll','swscale-8.dll'
        foreach ($name in $runtime) {
            Copy-PackageFile (Join-Path $source $name) (Join-Path $package "tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared\bin\$name")
        }
        Copy-PackageFile (Join-Path $assets 'recall.ico') (Join-Path $package 'recall-native\assets\recall.ico')
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = Join-Path $work 'payload.zip'
        [IO.Compression.ZipFile]::CreateFromDirectory($package, $zip)
        $arguments += "/resource:$zip,Recall.Payload.zip"
    }
    & powershell -NoProfile -File $nice $compiler @arguments
    if ($LASTEXITCODE -ne 0) { throw "Recall Setup compilation failed ($LASTEXITCODE)" }
    Get-Item -LiteralPath $OutputPath
} finally {
    if ($work -and (Test-Path -LiteralPath $work)) {
        $resolved = (Resolve-Path -LiteralPath $work).ProviderPath
        if ([IO.Path]::GetDirectoryName($resolved) -ne (Resolve-Path -LiteralPath $workspace).ProviderPath -or
            (Get-Item -LiteralPath $resolved).LinkType) { throw "Unexpected setup staging path: $resolved" }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
