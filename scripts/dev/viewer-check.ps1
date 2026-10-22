<#
  Viewer response checks for the editor's player (dev check; run after player changes).

  Each scenario opens a fresh hard link of a real clip (edits never touch the original), drives
  the editor with key presses, and reads the app's playback trace (RECALL_NATIVE_TRACE) to time
  responses and catch stalls. Prints one row per scenario and exits 1 if any fails.

    powershell -NoProfile -File scripts\dev\viewer-check.ps1 [-Clip <mp4 on L:>] [-Exe <recall-native.exe>]

  Needs a clip of at least 60 s. Default: the newest VALORANT clip in V:\ALL RECORDINGS, copied
  once to .tmp\viewer-check (hard links need the same volume as the copies).
#>
param([string]$Clip = '', [string]$Exe = '')
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$work = Join-Path $repo '.tmp\viewer-check'
New-Item -ItemType Directory -Force -Path $work | Out-Null
if (-not $Exe) {
  $Exe = @('target-gpui\release\recall-native.exe', 'target-gpui\debug\recall-native.exe') | ForEach-Object { Join-Path $repo $_ } | Where-Object { Test-Path $_ } | Select-Object -First 1
}
$base = Join-Path $work 'base.mp4'
if ($Clip) { Copy-Item $Clip $base -Force }
elseif (-not (Test-Path $base)) {
  $src = Get-ChildItem 'V:\ALL RECORDINGS' -Filter 'VALORANT__*.mp4' | Where-Object { $_.Length -gt 100MB } | Sort-Object LastWriteTime -Descending | Select-Object -First 1
  Copy-Item $src.FullName $base
}

Add-Type @'
using System; using System.Runtime.InteropServices;
public static class VC {
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
}
'@
$vk = @{ space = 0x20; i = 0x49; o = 0x4F; s = 0x53; l = 0x4C; j = 0x4A; k = 0x4B; home = 0x24; end = 0x23; delete = 0x2E }

function Start-Viewer([string]$name) {
  $clip = Join-Path $work "$name.mp4"
  Remove-Item $clip, "$clip.recall-edit.json" -ErrorAction SilentlyContinue
  New-Item -ItemType HardLink -Path $clip -Target $base | Out-Null
  $trace = Join-Path $work "$name.jsonl"
  Remove-Item $trace -ErrorAction SilentlyContinue
  $env:RECALL_NATIVE_TRACE = $trace; $env:RECALL_NATIVE_CLIP = $clip; $env:RECALL_NATIVE_WINDOW_SIZE = '1280x820'
  $env:RECALL_NO_CONTROLLER = '1'; $env:RECALL_CPU_MASK = 'ffff0000'
  $p = Start-Process -FilePath $Exe -WorkingDirectory (Split-Path $Exe) -PassThru
  foreach ($v in 'RECALL_NATIVE_TRACE', 'RECALL_NATIVE_CLIP', 'RECALL_NATIVE_WINDOW_SIZE', 'RECALL_NO_CONTROLLER') { Remove-Item "Env:$v" -ErrorAction SilentlyContinue }
  $t = Get-Date
  while (-not $p.MainWindowHandle -or $p.MainWindowHandle -eq 0) { Start-Sleep -Milliseconds 50; $p.Refresh(); if (((Get-Date) - $t).TotalSeconds -gt 20) { throw 'no window' } }
  $s = [pscustomobject]@{ Process = $p; Hwnd = $p.MainWindowHandle; Trace = $trace; Clip = $clip }
  $ready = Wait-Trace $s { param($r) $r.ready -and $r.clip } 30000
  if (-not $ready) { throw "${name}: preview never became ready" }
  $s | Add-Member OpenMs ([int]((Get-Date) - $t).TotalMilliseconds)
  $s
}
function Stop-Viewer($s) {
  if (-not $s.Process.HasExited) { [void]$s.Process.CloseMainWindow(); [void]$s.Process.WaitForExit(5000) }
}
# The newest complete trace line (the app may be mid-write on the last one).
function Last($s) {
  $lines = @(Get-Content $s.Trace -Tail 2 -ErrorAction SilentlyContinue)
  for ($n = $lines.Count - 1; $n -ge 0; $n--) { try { return ($lines[$n] | ConvertFrom-Json) } catch { } }
  $null
}
function Wait-Trace($s, [scriptblock]$cond, [int]$timeoutMs) {
  $t = Get-Date
  while (((Get-Date) - $t).TotalMilliseconds -lt $timeoutMs) {
    $r = Last $s
    if ($r -and (& $cond $r)) { return $r }
    Start-Sleep -Milliseconds 15
  }
  $null
}
function Key($s, [string]$name, [int]$times = 1) {
  for ($n = 0; $n -lt $times; $n++) {
    [void][VC]::PostMessage($s.Hwnd, 0x100, [IntPtr]$vk[$name], [IntPtr]1)
    [void][VC]::PostMessage($s.Hwnd, 0x101, [IntPtr]$vk[$name], [IntPtr]0xC0000001)
    Start-Sleep -Milliseconds 60
  }
}
# Plays for $secs from now; returns start→first-advance latency, audio/video progress and the worst A/V gap.
function Measure-Play($s, [double]$secs) {
  $before = Last $s
  $t = Get-Date
  Key $s 'space'
  $moved = Wait-Trace $s { param($r) $r.playing -and $r.audio -gt $before.audio + 0.05 -and $r.video -gt $before.video + 0.05 } 4000
  $latency = if ($moved) { [int]((Get-Date) - $t).TotalMilliseconds } else { -1 }
  Start-Sleep -Milliseconds ([int]($secs * 1000))
  $now = (Last $s).ms
  $rows = Get-Content $s.Trace | Select-Object -Last 400 | ForEach-Object { try { $_ | ConvertFrom-Json } catch { } } | Where-Object { $_ -and $_.ms -ge $now - $secs * 1000 + 500 -and $_.playing }
  $gap = ($rows | ForEach-Object { [math]::Abs($_.video - $_.audio) } | Measure-Object -Maximum).Maximum
  $after = Last $s
  [pscustomobject]@{ LatencyMs = $latency; Audio = $after.audio - $before.audio; Video = $after.video - $before.video; MaxGap = $gap; Seeks = $after.drift_seeks - $before.drift_seeks; From = $before.audio; Playing = $after.playing; Last = $after }
}

$results = @()
function Add-Result([string]$name, [bool]$pass, [string]$detail) { $script:results += [pscustomobject]@{ Scenario = $name; Result = $(if ($pass) { 'PASS' } else { 'FAIL' }); Detail = $detail } }

# 1. Open and play from the start.
$s = Start-Viewer 'play-start'
$m = Measure-Play $s 4
Add-Result 'open + play from 0' ($m.LatencyMs -ge 0 -and $m.LatencyMs -lt 600 -and $m.Video -gt 3.2 -and $m.MaxGap -lt 0.2) ("open {0} ms, start {1} ms, +{2:N2}s audio / +{3:N2}s video, worst gap {4:N3}s, {5} re-seeks" -f $s.OpenMs, $m.LatencyMs, $m.Audio, $m.Video, $m.MaxGap, $m.Seeks)
Stop-Viewer $s

# 2. Trim the start like a real edit (in-point at ~105 s), then play the kept part.
$s = Start-Viewer 'trim-start'
Key $s 'l' 21; Start-Sleep -Milliseconds 400; Key $s 'i'; Start-Sleep -Milliseconds 300; Key $s 'home'; Start-Sleep -Milliseconds 500
$m = Measure-Play $s 4
$start = (Last $s).ranges[0][0]
Add-Result 'trim start, play kept part' ($m.LatencyMs -ge 0 -and $m.LatencyMs -lt 800 -and $m.Video -gt 3.2 -and $m.MaxGap -lt 0.2 -and $m.From -ge $start - 0.05) ("kept from {0:N2}s, start {1} ms, +{2:N2}s audio / +{3:N2}s video, worst gap {4:N3}s, {5} re-seeks" -f $start, $m.LatencyMs, $m.Audio, $m.Video, $m.MaxGap, $m.Seeks)
Stop-Viewer $s

# 3. Jump while playing (+5 s) and measure how fast picture and sound follow.
$s = Start-Viewer 'seek-playing'
Key $s 'l' 8; Start-Sleep -Milliseconds 400
[void](Measure-Play $s 1.5)
$before = Last $s; $t = Get-Date
Key $s 'l'
$hit = Wait-Trace $s { param($r) $r.audio -gt $before.audio + 4.5 -and [math]::Abs($r.video - $r.audio) -lt 0.15 } 3000
$ms = if ($hit) { [int]((Get-Date) - $t).TotalMilliseconds } else { -1 }
Start-Sleep -Seconds 2
$after = Last $s
Add-Result 'jump +5 s while playing' ($ms -ge 0 -and $ms -lt 700 -and $after.playing -and $after.video - $hit.video -gt 1.5) ("picture+sound settled in {0} ms, then +{1:N2}s video in 2 s" -f $ms, $(if ($hit) { $after.video - $hit.video } else { 0 }))
Stop-Viewer $s

# 4. Play into the end: stops at the end of the clip.
$s = Start-Viewer 'play-end'
Key $s 'end'; Start-Sleep -Milliseconds 300; Key $s 'j'; Start-Sleep -Milliseconds 400
$end = (Last $s).ranges[-1][1]
Key $s 'space'
$stopped = Wait-Trace $s { param($r) -not $r.playing -and $r.audio -gt $end - 0.3 } 9000
Add-Result 'play to the end' ([bool]$stopped) ($(if ($stopped) { "stopped at {0:N2}s of {1:N2}s" -f $stopped.audio, $end } else { "still playing / stuck at {0:N2}s" -f (Last $s).audio }))
# 6. Play again after the end: starts over from the first kept frame.
$t = Get-Date
Key $s 'space'
$again = Wait-Trace $s { param($r) $r.playing -and $r.audio -gt 0.3 -and $r.audio -lt 5 -and $r.video -gt 0.3 } 4000
Add-Result 'replay after the end' ([bool]$again) ($(if ($again) { "restarted from the start, moving after {0} ms" -f [int]((Get-Date) - $t).TotalMilliseconds } else { "stuck at {0:N2}s" -f (Last $s).audio }))
Stop-Viewer $s

# 7. Pause and resume mid-clip: resumes where it paused.
$s = Start-Viewer 'pause-resume'
Key $s 'l' 6; Start-Sleep -Milliseconds 300
[void](Measure-Play $s 1.5)
Key $s 'k'; Start-Sleep -Milliseconds 700
$paused = Last $s
$m = Measure-Play $s 2
Add-Result 'pause, then resume' (-not $paused.playing -and $m.LatencyMs -ge 0 -and $m.LatencyMs -lt 600 -and [math]::Abs($m.From - $paused.audio) -lt 0.1 -and $m.Video -gt 1.5) ("paused at {0:N2}s, resumed in {1} ms, +{2:N2}s video" -f $paused.audio, $m.LatencyMs, $m.Video)
Stop-Viewer $s

# 5. Drag the start trim handle with the mouse to ~104 s (the preview follows the drag), then play.
#    Layout at 1280x820: video track row y=678, track from x=98 (start handle) to x=1255.
$s = Start-Viewer 'drag-trim'
$dur = (Last $s).ranges[-1][1]
$x0 = 99; $x1 = [int](98 + (1255 - 98) * 104.0 / $dur); $y = 678
function MouseL($x, $yy) { [IntPtr](($yy -shl 16) -bor ($x -band 0xFFFF)) }
[void][VC]::PostMessage($s.Hwnd, 0x200, [IntPtr]0, (MouseL $x0 $y)); Start-Sleep -Milliseconds 60
[void][VC]::PostMessage($s.Hwnd, 0x201, [IntPtr]1, (MouseL $x0 $y)); Start-Sleep -Milliseconds 30
for ($n = 1; $n -le 40; $n++) { $x = [int]($x0 + ($x1 - $x0) * $n / 40); [void][VC]::PostMessage($s.Hwnd, 0x200, [IntPtr]1, (MouseL $x $y)); Start-Sleep -Milliseconds 16 }
[void][VC]::PostMessage($s.Hwnd, 0x202, [IntPtr]0, (MouseL $x1 $y)); Start-Sleep -Milliseconds 600
$start = (Last $s).ranges[0][0]
$m = Measure-Play $s 4
Add-Result 'drag trim handle, play' ($start -gt 90 -and $m.LatencyMs -ge 0 -and $m.LatencyMs -lt 800 -and $m.Video -gt 3.2 -and $m.MaxGap -lt 0.2) ("kept from {0:N2}s, start {1} ms, +{2:N2}s audio / +{3:N2}s video, worst gap {4:N3}s, {5} re-seeks" -f $start, $m.LatencyMs, $m.Audio, $m.Video, $m.MaxGap, $m.Seeks)
Stop-Viewer $s

$results | Format-Table -AutoSize | Out-String -Width 200
if ($results.Result -contains 'FAIL') { exit 1 }
