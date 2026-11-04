<#
  Cost trace: a Windows Performance Recorder trace (CPU stacks + GPU) of the engine while it
  records the dev capture target, for flame graphs per process (engine, app, overlay, dwm).

  Run from an ELEVATED PowerShell (WPR needs admin), with no game running:
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts\dev\cost-trace.ps1 -Engine <recall.exe>

  It refuses while a game runs. It starts WPR (profiles CPU and GPU, file mode), runs
  scripts\dev\engine-cost.ps1 for -Seconds with a -SaveSeconds save at the end (so the save's
  audio encode is in the trace), then stops WPR into -Out. The test clips go to a temp folder
  that is deleted afterwards. No real mic, no visible window beyond engine-cost's one pixel.
  The trace (about 0.5-2 GB) is opened later in WPA or PerfView; nothing here uploads it.
#>
param(
  [string]$Engine = 'L:\General\.tmp\recall-low-end-target\release\recall.exe',
  [double]$Seconds = 60,
  [int]$SaveSeconds = 30,
  [string]$Out = 'L:\General\.tmp\recall-low-end-logs\cost-trace.etl'
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) { Write-Error 'Run this from an elevated PowerShell: WPR needs admin.'; exit 2 }
$games = 'VALORANT-Win64-Shipping', 'cs2', 'RocketLeague', 'FortniteClient-Win64-Shipping', 'r5apex', 'r5apex_dx12', 'Overwatch'
$running = @(Get-Process -Name $games -ErrorAction SilentlyContinue | ForEach-Object ProcessName)
if ($running) { Write-Error "A game is running ($($running -join ', ')); run the trace later."; exit 3 }
foreach ($f in $Engine, $Target) { if (-not (Test-Path $f)) { Write-Error "Not found: $f"; exit 4 } }

# The engine needs the FFmpeg DLLs: the main checkout keeps them (worktrees do not).
$ff = Join-Path $repo 'tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared\bin'
if (-not (Test-Path $ff)) { $ff = 'L:\General\recall\tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared\bin' }
$env:PATH = "$ff;$env:PATH"

$clips = Join-Path $env:TEMP "recall-cost-trace-$PID"
New-Item -ItemType Directory -Force $clips | Out-Null
New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
& wpr -cancel 2>$null | Out-Null
& wpr -start CPU -start GPU -filemode
if ($LASTEXITCODE -ne 0) { Write-Error "wpr -start failed ($LASTEXITCODE)"; exit 5 }
try {
  & powershell -NoProfile -File (Join-Path $repo 'scripts\dev\engine-cost.ps1') -Engine $Engine -Target $Target -Seconds $Seconds -SaveSeconds $SaveSeconds -OutDir $clips
  "engine-cost exit $LASTEXITCODE"
} finally {
  & wpr -stop $Out
  "wpr stop exit $LASTEXITCODE -> $Out"
  Remove-Item -Recurse -Force $clips -ErrorAction SilentlyContinue
}
