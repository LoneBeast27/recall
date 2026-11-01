<#
  Engine cost check: what recording costs the PC, measured the way the game sees it.

  Starts the dev capture target (examples\present_target.rs: a topmost 2560x1440 window with one
  black pixel on screen at the bottom-left corner, presenting a moving test pattern at -TargetFps,
  like a game at 500+ fps), starts the
  engine on that window exactly as the controller does for a game (window capture, CCD1 mask,
  BelowNormal, Moments off), lets it warm up, then reads the engine's CPU over -Seconds:
  CPU-seconds per minute, cores, and CPU-ms per captured frame, plus a per-thread table.
  Optional -SaveSeconds N saves a clip at the end (for frame / colour checks).
  Exit 1 when CPU-ms per captured frame is above -MaxMsPerFrame (the regression gate).

  Never run while a game is running (it refuses): it costs CPU/GPU and starts an NVENC session.
  Only that one corner pixel is on screen. Clips go to -OutDir (default .tmp\engine-cost\clips).
  The Mic track records -TestMic, an endpoint nobody speaks into, and never falls back to the real
  mic (RECALL_AUDIO_EXACT). The run fails when that track does not start: the cost would not
  include it.

    powershell -NoProfile -File scripts\dev\engine-cost.ps1 [-Engine <recall.exe>] [-Seconds 30]
      [-TargetFps 500] [-MaxMsPerFrame 1.5] [-SaveSeconds 0] [-Json <file>] [-TestMic <name words>]
#>
param(
  [string]$Engine = '',
  [string]$Target = '',
  [double]$Seconds = 30,
  [double]$Warmup = 8,
  [int]$TargetFps = 500,
  [int]$TargetX = -2559,
  [int]$TargetY = 1439,
  [string]$TargetArgs = '',
  [switch]$RefuseGpu,
  [double]$MaxMsPerFrame = 1.5,
  [int]$SaveSeconds = 0,
  [string]$OutDir = '',
  [string]$Json = '',
  [string]$TestMic = 'microphone steam streaming',
  [switch]$Monitor
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
if (-not $Engine) { $Engine = Join-Path $repo 'target-dev\release\recall.exe' }
if (-not $Target) { $Target = Join-Path $repo 'target-dev\release\examples\present_target.exe' }
if (-not $OutDir) { $OutDir = Join-Path $repo '.tmp\engine-cost\clips' }
foreach ($f in @($Engine, $Target)) { if (-not (Test-Path $f)) { throw "missing $f (build it: cargo build --release -p recall --example present_target)" } }
. (Join-Path $PSScriptRoot 'thread-cpu.ps1')

$games = Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -match '^(VALORANT-Win64-Shipping|RocketLeague|streaming_client)$' }
if ($games) { throw ("a game is running (" + (($games | ForEach-Object ProcessName) -join ', ') + "): not measuring now") }

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$ffbin = Join-Path $repo 'tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared\bin'

function Start-Child([string]$exe, [string]$argline, [hashtable]$envs) {
  $psi = [Diagnostics.ProcessStartInfo]::new($exe, $argline)
  $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
  $psi.RedirectStandardOutput = $true; $psi.RedirectStandardInput = $true; $psi.RedirectStandardError = $true
  $psi.WorkingDirectory = Split-Path $exe
  foreach ($k in $envs.Keys) { $psi.EnvironmentVariables[$k] = $envs[$k] }
  $p = [Diagnostics.Process]::new(); $p.StartInfo = $psi
  $q = [Collections.Concurrent.ConcurrentQueue[string]]::new()
  $e = [Collections.Concurrent.ConcurrentQueue[string]]::new()
  Register-ObjectEvent -InputObject $p -EventName OutputDataReceived -MessageData $q -Action { if ($EventArgs.Data) { $Event.MessageData.Enqueue($EventArgs.Data) } } | Out-Null
  Register-ObjectEvent -InputObject $p -EventName ErrorDataReceived -MessageData $e -Action { if ($EventArgs.Data) { $Event.MessageData.Enqueue($EventArgs.Data) } } | Out-Null
  [void]$p.Start(); $p.BeginOutputReadLine(); $p.BeginErrorReadLine()
  # The first write carries a UTF-8 BOM; spend it on a blank line the engine ignores.
  $p.StandardInput.WriteLine(''); $p.StandardInput.Flush()
  [pscustomobject]@{ proc = $p; out = $q; err = $e }
}
function Drain($queue) { $lines = @(); $l = $null; while ($queue.TryDequeue([ref]$l)) { $lines += $l }; $lines }

$tgt = Start-Child $Target "--fps $TargetFps --x $TargetX --y $TargetY $TargetArgs" @{}
$hwnd = 0; $deadline = (Get-Date).AddSeconds(15); $tlines = @()
while (-not $hwnd -and (Get-Date) -lt $deadline) {
  Start-Sleep -Milliseconds 200
  foreach ($l in (Drain $tgt.out)) { $tlines += $l; if ($l -match '"hwnd":(\d+)') { $hwnd = [int64]$Matches[1] } }
}
if (-not $hwnd) { $tgt.proc.Kill(); throw "present_target gave no window: $($tlines -join ' | ')" }

$envs = @{
  RECALL_CAPTURE_NAME = 'PresentTarget'; RECALL_AUDIO_GAME_PID = "$($tgt.proc.Id)"
  RECALL_AUDIO_MIC = $TestMic; RECALL_AUDIO_EXACT = '1'
  RECALL_CPU_MASK = 'ffff0000'; RECALL_FPS = '60'; RECALL_BITRATE = '25000000'; RECALL_BUFFER_SECS = '120'
  RECALL_CODEC = 'h264'; RECALL_QUALITY = 'high'; RECALL_OUT_DIR = $OutDir; RECALL_STAGING_DIR = 'off'
  RECALL_MOMENTS = '0'; RECALL_MOMENTS_DB = (Join-Path $OutDir 'no-moments.db')
  PATH = "$ffbin;$env:PATH"
}
if (-not $Monitor) { $envs.RECALL_CAPTURE_HWND = "$hwnd" }
# Exercise the CPU fallback: NVENC refuses GPU frames, capture must switch and keep recording.
if ($RefuseGpu) { $envs.RECALL_REFUSE_GPU_FRAMES = '1' }
$eng = Start-Child $Engine '--json' $envs
$log = [Collections.Generic.List[string]]::new()
function Pump { foreach ($l in (Drain $eng.out)) { $log.Add($l) }; foreach ($l in (Drain $eng.err)) { $log.Add("stderr: $l") } }

$warm = (Get-Date).AddSeconds($Warmup)
while ((Get-Date) -lt $warm) { Start-Sleep -Milliseconds 250; Pump; if ($eng.proc.HasExited) { break } }
if ($eng.proc.HasExited) { $tgt.proc.Kill(); throw "engine exited during warmup: $($log -join "`n")" }
$micUp = $log | Where-Object { $_ -match "audio track 'Mic' up" } | Select-Object -First 1
if (-not $micUp) {
  $eng.proc.StandardInput.WriteLine('{"cmd":"quit"}'); [void]$eng.proc.WaitForExit(8000); if (-not $tgt.proc.HasExited) { $tgt.proc.Kill() }
  throw "no test Mic track on '$TestMic': $(($log | Where-Object { $_ -match "'Mic'" }) -join ' | ')"
}

# A target WGC sees as occluded delivers no frames; say so instead of measuring an idle engine.
$warmFps = @($log | Where-Object { $_ -match '"type":"status"' } | ForEach-Object { ($_ | ConvertFrom-Json).fps } | Select-Object -Last 2)
if (-not ($warmFps | Where-Object { $_ -gt 0 })) {
  $eng.proc.StandardInput.WriteLine('{"cmd":"quit"}'); [void]$eng.proc.WaitForExit(8000); if (-not $tgt.proc.HasExited) { $tgt.proc.Kill() }
  throw "no frames captured during warmup (target occluded?): $(($log | Where-Object { $_ -notmatch 'status|endpoints' } | Select-Object -Last 12) -join ' | ')"
}
$before = $log.Count
$m = Measure-ThreadCpu $eng.proc.Id $Seconds
Pump
$fpsVals = @($log | Select-Object -Skip $before | Where-Object { $_ -match '"type":"status"' } | ForEach-Object { ($_ | ConvertFrom-Json).fps } | Where-Object { $_ -gt 0 })
$fps = if ($fpsVals) { ($fpsVals | Measure-Object -Average).Average } else { 0 }
$tp = @(Drain $tgt.out | Where-Object { $_ -match 'presented' } | ForEach-Object { ($_ | ConvertFrom-Json).presented })
$targetFps = if ($tp) { ($tp | Select-Object -Last 10 | Measure-Object -Average).Average } else { 0 }

$saved = $null
if ($SaveSeconds -gt 0) {
  $eng.proc.StandardInput.WriteLine("{`"cmd`":`"save_clip`",`"seconds`":$SaveSeconds}"); $eng.proc.StandardInput.Flush()
  $deadline = (Get-Date).AddSeconds(20)
  while (-not $saved -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 250; Pump
    $hit = $log | Where-Object { $_ -match '"type":"(saved|save_failed)"' } | Select-Object -Last 1
    if ($hit) { $saved = $hit | ConvertFrom-Json }
  }
}
$eng.proc.StandardInput.WriteLine('{"cmd":"quit"}'); $eng.proc.StandardInput.Flush()
if (-not $eng.proc.WaitForExit(8000)) { $eng.proc.Kill() }
if (-not $tgt.proc.HasExited) { $tgt.proc.Kill() }
Pump

$frames = $fps * $m.wall_s
$msPerFrame = if ($frames -gt 0) { $m.process_cpu_ms / $frames } else { [double]::PositiveInfinity }
$groups = $m.threads | Group-Object { if ($_.name) { $_.name } else { "<$($_.module)>" } } | ForEach-Object {
  [pscustomobject]@{
    thread = $_.Name; count = $_.Count
    user_ms = [math]::Round(($_.Group | Measure-Object user_ms -Sum).Sum, 0)
    kernel_ms = [math]::Round(($_.Group | Measure-Object kernel_ms -Sum).Sum, 0)
    total_ms = [math]::Round(($_.Group | Measure-Object total_ms -Sum).Sum, 0)
  }
} | Sort-Object total_ms -Descending
$result = [pscustomobject]@{
  engine = $Engine; seconds = $m.wall_s; target_presented_fps = [math]::Round($targetFps, 0)
  captured_fps = [math]::Round($fps, 1); cpu_ms = $m.process_cpu_ms; cores = $m.cores
  cpu_s_per_min = $m.cpu_s_per_min; cpu_ms_per_frame = [math]::Round($msPerFrame, 2)
  gate_ms_per_frame = $MaxMsPerFrame; pass = ($msPerFrame -le $MaxMsPerFrame -and $fps -ge 30)
  saved = $saved; threads = @($groups)
  raw_threads = @($m.threads | Where-Object { $_.total_ms -gt 0 })
  engine_log = @($log | Where-Object { $_ -notmatch '"type":"status"' } | Select-Object -Last 40)
}
"engine: $Engine"
"target presented $($result.target_presented_fps) fps; engine captured $($result.captured_fps) fps over $($m.wall_s) s"
"engine CPU: $($m.process_cpu_ms) ms = $($m.cores) cores = $($m.cpu_s_per_min) CPU-s/min = $($result.cpu_ms_per_frame) CPU-ms per captured frame (gate $MaxMsPerFrame)"
$groups | Select-Object -First 14 | Format-Table -AutoSize | Out-String -Width 200
if ($saved) { "saved: $($saved | ConvertTo-Json -Compress)" }
if ($Json) { $result | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 $Json }
if (-not $result.pass) { "FAIL: engine cost regressed (or no frames captured)"; exit 1 }
"PASS"
