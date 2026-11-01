<#
  Driver for scripts\dev\viewer-check.ps1: starts private Recall instances and drives them the
  way a person does, without touching the desktop. Dot-source it, then call Initialize-ViewerCheck.

  Every instance runs outside every monitor, out of the taskbar, never activates and plays
  silence (RECALL_NATIVE_OFFSCREEN), with its
  own settings folder, library folder and control pipe, so the person's Recall window, recorder,
  library titles and player log are never touched. Input is posted to the instance's own window
  (PostMessage), never injected into the desktop: keys go through the app's key handler, clicks
  land on the buttons a person clicks (their painted bounds come from the app's trace).
#>

Add-Type @'
using System; using System.IO; using System.Runtime.InteropServices;
public static class VC {
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);
  // Silence holes (>= 8 zero frames) inside sound in a 16-bit preview stem: the 2026-10-27 choppy audio.
  public static int Holes(string wav) {
    if (wav.StartsWith(@"\\?\")) wav = wav.Substring(4); // Windows PowerShell rejects verbatim paths
    byte[] b = File.ReadAllBytes(wav); int ch = BitConverter.ToInt16(b, 22); int n = (b.Length - 44) / (2 * ch);
    Func<int, int> peak = f => { int m = 0; for (int c = 0; c < ch; c++) m = Math.Max(m, Math.Abs((int)BitConverter.ToInt16(b, 44 + 2 * (f * ch + c)))); return m; };
    Func<int, int, bool> loud = (a, z) => { for (int f = Math.Max(0, a); f < Math.Min(n, z); f++) if (peak(f) > 300) return true; return false; };
    int holes = 0, run = 0;
    for (int f = 0; f < n; f++) {
      if (peak(f) == 0) { run++; continue; }
      if (run >= 8 && loud(f - run - 48, f - run) && loud(f, f + 48)) holes++;
      run = 0;
    }
    return holes;
  }
}
'@
$script:vk = @{ space = 0x20; i = 0x49; o = 0x4F; c = 0x43; s = 0x53; l = 0x4C; j = 0x4A; k = 0x4B; home = 0x24; end = 0x23; delete = 0x2E; left = 0x25; right = 0x27; escape = 0x1B; enter = 0x0D; tab = 0x09; a = 0x41; b = 0x42; d1 = 0x31; d2 = 0x32; d3 = 0x33; slash = 0xBF }

function Initialize-ViewerCheck([string]$Repo, [string]$Exe) {
  # Children inherit this: a missing DLL fails the scenario, not in a dialog on the desktop.
  [void][VC]::SetErrorMode(0x8003)
  $script:work = Join-Path $Repo '.tmp\viewer-check'
  $script:sandbox = Join-Path $script:work 'appdata'
  # Private cache: prepared stems are reused across runs, saved edits (projects) never are.
  $script:cache = Join-Path $script:work 'cache'
  Remove-Item (Join-Path $script:cache 'projects') -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force -Path $script:work, $script:sandbox, $script:cache | Out-Null
  $script:ffmpeg = (Get-Command ffmpeg.exe -ErrorAction SilentlyContinue).Source
  $script:ffprobe = (Get-Command ffprobe.exe -ErrorAction SilentlyContinue).Source
  if (-not $Exe) {
    $Exe = @('target-gpui\release\recall-native.exe', 'target-gpui\debug\recall-native.exe') | ForEach-Object { Join-Path $Repo $_ } | Where-Object { Test-Path $_ } | Select-Object -First 1
  }
  $script:exe = $Exe
  $script:results = @()
}

# A fresh hard link of $from named $name in $dir (edits never touch the original).
function New-ClipLink([string]$dir, [string]$name, [string]$from) {
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $clip = Join-Path $dir "$name.mp4"
  if ([IO.Path]::GetFullPath($clip) -eq [IO.Path]::GetFullPath($from)) { throw "scenario clip '$name' would replace its own source" }
  Remove-Item $clip, "$clip.recall-edit.json" -ErrorAction SilentlyContinue
  New-Item -ItemType HardLink -Path $clip -Target $from | Out-Null
  $clip
}

# Opens a hard link of $from in the editor. -Library starts on the Library page instead, with the
# clip (and the -Also clips) as the only clips in a private library folder.
function Start-Viewer([string]$name, [string]$from, [switch]$Library, [string[]]$Also = @()) {
  # Editor runs get an empty library, so the smart cache never prepares the clip being opened.
  $libDir = Join-Path $script:work $(if ($Library) { "lib-$name" } else { 'lib-empty' })
  # Titles and favourites live in the sandbox's library file, by clip name: each run starts clean.
  if ($Library) { Remove-Item $libDir -Recurse -Force -ErrorAction SilentlyContinue; Remove-Item (Join-Path $script:sandbox 'library.json') -ErrorAction SilentlyContinue }
  New-Item -ItemType Directory -Force -Path $libDir | Out-Null
  $clip = New-ClipLink $(if ($Library) { $libDir } else { $script:work }) $name $from
  foreach ($extra in $Also) { [void](New-ClipLink $libDir $extra $from) }
  $trace = Join-Path $script:work "$name.jsonl"
  Remove-Item $trace -ErrorAction SilentlyContinue
  $env:RECALL_NATIVE_TRACE = $trace; $env:RECALL_NATIVE_WINDOW_SIZE = $(if ($script:windowSize) { $script:windowSize } else { '1280x820' }); $env:RECALL_NATIVE_OFFSCREEN = '1'
  if (-not $Library) { $env:RECALL_NATIVE_CLIP = $clip }
  $env:RECALL_NO_CONTROLLER = '1'; $env:RECALL_CPU_MASK = 'ffff0000'; $env:RECALL_CONTROL_PIPE = "recall-viewer-check-$PID"
  $env:RECALL_OUT_DIR = $libDir; $env:RECALL_FOLDERS = Join-Path $script:sandbox 'folders.json'
  $env:RECALL_LIBRARY_META = Join-Path $script:sandbox 'library.json'; $env:RECALL_CACHE_DIR = $script:cache
  if ($script:ffmpeg) { $env:RECALL_FFMPEG = $script:ffmpeg }
  if ($script:ffprobe) { $env:RECALL_FFPROBE = $script:ffprobe }
  $realAppData = $env:APPDATA; $env:APPDATA = $script:sandbox
  try { $p = Start-Process -FilePath $script:exe -WorkingDirectory (Split-Path $script:exe) -NoNewWindow -PassThru; [void]$p.Handle } # a console exe: on our console, no window of its own; keeps the exit code readable
  finally {
    $env:APPDATA = $realAppData
    foreach ($v in 'RECALL_NATIVE_TRACE', 'RECALL_NATIVE_CLIP', 'RECALL_NATIVE_WINDOW_SIZE', 'RECALL_NATIVE_OFFSCREEN', 'RECALL_NO_CONTROLLER', 'RECALL_CONTROL_PIPE', 'RECALL_OUT_DIR', 'RECALL_FOLDERS', 'RECALL_LIBRARY_META', 'RECALL_CACHE_DIR', 'RECALL_FFMPEG', 'RECALL_FFPROBE') { Remove-Item "Env:$v" -ErrorAction SilentlyContinue }
  }
  $t = Get-Date
  while (-not $p.MainWindowHandle -or $p.MainWindowHandle -eq 0) { Start-Sleep -Milliseconds 50; $p.Refresh(); if (((Get-Date) - $t).TotalSeconds -gt 20) { $how = if ($p.HasExited) { "exit 0x{0:x8}" -f $p.ExitCode } else { 'still running'; Stop-Process -Id $p.Id -Force }; throw "no window ($how)" } }
  $s = [pscustomobject]@{ Process = $p; Hwnd = $p.MainWindowHandle; Trace = $trace; Clip = $clip; Name = $name }
  $want = if ($Library) { { param($r) $r.route -eq 'library' -and $r.targets.PSObject.Properties.Name -contains "tile:$name.mp4" } } else { { param($r) $r.ready -and $r.clip -and $r.targets.play } }
  if (-not (Wait-Trace $s $want 60000)) { Stop-Viewer $s; throw "${name}: never became ready" }
  $s | Add-Member OpenMs ([int]((Get-Date) - $t).TotalMilliseconds)
  $s
}

function Stop-Viewer($s) {
  if (-not $s.Process.HasExited) { [void]$s.Process.CloseMainWindow(); [void]$s.Process.WaitForExit(5000) }
  if (-not $s.Process.HasExited) { Stop-Process -Id $s.Process.Id -Force } # our own instance only
}

# The newest complete trace line (the app may be mid-write on the last one).
function Last($s) {
  $lines = @(Get-Content $s.Trace -Tail 3 -ErrorAction SilentlyContinue)
  for ($n = $lines.Count - 1; $n -ge 0; $n--) { try { $r = $lines[$n] | ConvertFrom-Json; if ($null -ne $r.ms -and $r.route) { return $r } } catch { } }
  $null
}
# Trace lines newer than $sinceMs (the app's own clock).
function Rows($s, [double]$sinceMs = 0, [int]$tail = 600) {
  Get-Content $s.Trace -Tail $tail | ForEach-Object { try { $_ | ConvertFrom-Json } catch { } } | Where-Object { $_ -and $_.route -and $_.ms -ge $sinceMs }
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
# Waits until the app has polled at least once more (its state reflects everything posted so far).
function Settle($s, [int]$ms = 120) {
  Start-Sleep -Milliseconds $ms
  $now = (Last $s).ms
  [void](Wait-Trace $s { param($r) $r.ms -gt $now + 30 } 2000)
  Last $s
}

function Key($s, [string]$name, [int]$times = 1, [int]$gapMs = 60) {
  for ($n = 0; $n -lt $times; $n++) {
    [void][VC]::PostMessage($s.Hwnd, 0x100, [IntPtr]$script:vk[$name], [IntPtr]1)
    [void][VC]::PostMessage($s.Hwnd, 0x101, [IntPtr]$script:vk[$name], [IntPtr]0xC0000001)
    Start-Sleep -Milliseconds $gapMs
  }
}
function Point([int]$x, [int]$y) { [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF)) }
function Mouse($s, [uint32]$msg, [int]$x, [int]$y, [int]$buttons) { [void][VC]::PostMessage($s.Hwnd, $msg, [IntPtr]$buttons, (Point $x $y)) }
# Wheel notches at a client point (the message carries screen coordinates).
function Wheel($s, [int]$x, [int]$y, [int]$delta, [int]$times = 1) {
  $p = New-Object VC+POINT; $p.X = $x; $p.Y = $y
  [void][VC]::ClientToScreen($s.Hwnd, [ref]$p)
  for ($n = 0; $n -lt $times; $n++) { [void][VC]::PostMessage($s.Hwnd, 0x20A, [IntPtr](($delta -band 0xFFFF) -shl 16), (Point $p.X $p.Y)); Start-Sleep -Milliseconds 30 }
}
# Center of a painted control (device px), from the newest trace line.
function Target($s, [string]$name) {
  $r = Wait-Trace $s { param($r) $null -ne $r.targets.$name } 3000
  if (-not $r) { throw "$($s.Name): control '$name' is not on screen" }
  $b = $r.targets.$name
  [pscustomobject]@{ X = [int]($b[0] + $b[2] / 2); Y = [int]($b[1] + $b[3] / 2); Left = $b[0]; Top = $b[1]; Width = $b[2]; Height = $b[3] }
}
# A left click on a control, the way a person clicks it (move, press, release).
function Click($s, [string]$name) {
  $t = Target $s $name
  Mouse $s 0x200 $t.X $t.Y 0; Start-Sleep -Milliseconds 40
  Mouse $s 0x201 $t.X $t.Y 1; Start-Sleep -Milliseconds 40
  Mouse $s 0x202 $t.X $t.Y 0; Start-Sleep -Milliseconds 60
}
# X (device px) of source time $t on the timeline.
function TrackX($s, [double]$t) {
  $tracks = Target $s 'tracks'
  $f = [double]$t / [double](Last $s).duration # trace numbers are [decimal]: keep [math] on doubles
  [int]($tracks.Left + $tracks.Width * [math]::Min(1.0, [math]::Max(0.0, $f)))
}
# Press on the timeline at $from seconds, drag to $to seconds in $steps moves, release.
function Scrub($s, [double]$from, [double]$to, [int]$steps = 12) {
  $tracks = Target $s 'tracks'; $y = $tracks.Top + 30
  $x0 = TrackX $s $from; $x1 = TrackX $s $to
  Mouse $s 0x200 $x0 $y 0; Start-Sleep -Milliseconds 30
  Mouse $s 0x201 $x0 $y 1; Start-Sleep -Milliseconds 30
  for ($n = 1; $n -le $steps; $n++) { Mouse $s 0x200 ([int]($x0 + ($x1 - $x0) * $n / $steps)) $y 1; Start-Sleep -Milliseconds 16 }
  Mouse $s 0x202 $x1 $y 0; Start-Sleep -Milliseconds 60
}
# Drag a painted handle (trim-start / trim-end) to source time $to.
function DragHandle($s, [string]$handle, [double]$to, [int]$steps = 30) {
  $h = Target $s $handle; $x1 = TrackX $s $to
  Mouse $s 0x200 $h.X $h.Y 0; Start-Sleep -Milliseconds 40
  Mouse $s 0x201 $h.X $h.Y 1; Start-Sleep -Milliseconds 30
  for ($n = 1; $n -le $steps; $n++) { Mouse $s 0x200 ([int]($h.X + ($x1 - $h.X) * $n / $steps)) $h.Y 1; Start-Sleep -Milliseconds 16 }
  Mouse $s 0x202 $x1 $h.Y 0; Start-Sleep -Milliseconds 300
}
function Minimize($s) { [void][VC]::ShowWindow($s.Hwnd, 7) }  # SW_SHOWMINNOACTIVE
function Restore($s) { [void][VC]::ShowWindow($s.Hwnd, 4) }   # SW_SHOWNOACTIVATE

# Presses play ($how: space | button), then checks picture and sound both move for $secs:
# start latency, progress, the worst A/V gap, and whether the video player itself is running
# (a stopped video re-seeked to the sound every 500 ms used to read as "moving").
function Measure-Play($s, [double]$secs, [string]$how = 'space') {
  $before = Settle $s 60
  $t = Get-Date
  if ($how -eq 'button') { Click $s 'play' } else { Key $s 'space' }
  $moved = Wait-Trace $s { param($r) $r.playing -and [math]::Abs($r.pos - $before.pos) -gt 0.05 -and $r.video_state -eq 'playing' } 4000
  $latency = if ($moved) { [int]((Get-Date) - $t).TotalMilliseconds } else { -1 }
  $first = if ($moved) { $moved } else { Last $s }
  Start-Sleep -Milliseconds ([int]($secs * 1000))
  $after = Last $s
  $rows = @(Rows $s ($first.ms + 300) | Where-Object { $_.playing })
  $gap = ($rows | ForEach-Object { [math]::Abs($_.video - $_.audio) } | Measure-Object -Maximum).Maximum
  $videoRan = $rows.Count -gt 0 -and -not ($rows | Where-Object { $_.video_state -ne 'playing' })
  [pscustomobject]@{
    LatencyMs = $latency; Before = $before; First = $first; Last = $after; From = $first.pos
    Progress = $after.pos - $first.pos; Video = $after.video - $first.video; MaxGap = $gap; VideoRan = $videoRan
    Seeks = $after.drift_seeks - $before.drift_seeks; Playing = $after.playing
  }
}

# After something that should start playback: within 1.5 s the clock moves with the video player
# running, then for $secs the picture keeps up with the sound (no frozen picture, no picture left
# at an old spot). $from: where playback must start (null = anywhere).
function Check-Plays($s, [double]$secs = 1.5, $from = $null, [double]$sinceMs = -1) {
  if ($sinceMs -lt 0) { $sinceMs = (Last $s).ms }
  $go = Wait-Trace $s { param($r) $r.playing -and $r.video_state -eq 'playing' -and $r.ms -gt $sinceMs } 1500
  if (-not $go) { $r = Last $s; return [pscustomobject]@{ Pass = $false; Detail = "never played (playing=$($r.playing), video $($r.video_state) at $(Fmt $r.video), clock $(Fmt $r.pos), status '$($r.status)')" } }
  Start-Sleep -Milliseconds ([int]($secs * 1000))
  $rows = @(Rows $s ($go.ms - 1) | Where-Object { $_.playing })
  $after = $rows[-1]
  $start = [double]$rows[0].pos
  $late = @($rows | Where-Object { $_.ms -ge $go.ms + 150 })
  # While a clip loads, playback may wait for the part ahead: that wait is not a frozen picture.
  $resumed = @($rows | Where-Object { $_.loading.buffering } | ForEach-Object { [double]$_.ms })
  # The sound is stopped while buffering: the two clocks mean nothing until it carries on. After
  # that the WinMM position sits still for about 0.3 s and then jumps to the true time, so for
  # 0.4 s the clock reads behind a picture that is on time.
  $steady = @($late | Where-Object { $row = $_; -not $row.loading.buffering -and -not ($resumed | Where-Object { [double]$row.ms - $_ -lt 400 -and [double]$row.ms -gt $_ }) })
  $gap = [double](($steady | ForEach-Object { [math]::Abs([double]$_.video - [double]$_.pos) } | Measure-Object -Maximum).Maximum)
  $frozen = @($late | Where-Object { $row = $_; $row.video_state -ne 'playing' -and -not $row.loading.buffering -and -not ($resumed | Where-Object { [double]$row.ms - $_ -lt 120 -and [double]$row.ms -gt $_ }) }).Count
  $moved = [double]$after.pos - $start; $vmoved = [double]$after.video - [double]$rows[0].video
  $want = [math]::Min($secs, [double]$after.kept) * 0.6
  $okFrom = ($null -eq $from) -or ([math]::Abs($start - [double]$from) -lt 0.12)
  $pass = $okFrom -and $moved -gt $want -and $vmoved -gt $want -and $gap -lt 0.2 -and $frozen -eq 0
  $d = "from {0}s, +{1}s clock / +{2}s picture, worst A/V gap {3}s" -f (Fmt $start), (Fmt $moved), (Fmt $vmoved), (Fmt $gap)
  if ($frozen) { $d += ", picture not playing in $frozen samples" }
  if (-not $okFrom) { $d += " (wanted from $(Fmt $from)s)" }
  [pscustomobject]@{ Pass = $pass; Detail = $d; Start = $start; Last = $after }
}
# Presses play the way a person does ($how: space | button) and checks it plays (Check-Plays).
function Press-Play($s, [string]$how = 'space', [double]$secs = 1.5, $from = $null) {
  $t0 = (Settle $s 60).ms
  if ($how -eq 'button') { Click $s 'play' } else { Key $s $how }
  Check-Plays $s $secs $from $t0
}
# After a pause ($how: space | button | k): stopped within 0.6 s, and the clock and picture stay put.
function Press-Pause($s, [string]$how = 'space') {
  if ($how -eq 'button') { Click $s 'play' } else { Key $s $how }
  $stopped = Wait-Trace $s { param($r) -not $r.playing } 600
  if (-not $stopped) { return [pscustomobject]@{ Pass = $false; Detail = 'still playing 0.6 s after pause'; At = (Last $s).pos } }
  Start-Sleep -Milliseconds 250; $a = Last $s; Start-Sleep -Milliseconds 500; $b = Last $s
  $still = [math]::Abs([double]$b.pos - [double]$a.pos) -lt 0.01 -and [math]::Abs([double]$b.video - [double]$a.video) -lt 0.02 -and $b.video_state -ne 'playing'
  [pscustomobject]@{ Pass = $still; Detail = "paused at $(Fmt $b.pos)s$(if (-not $still) { ' but still moving' })"; At = [double]$b.pos }
}
# The picture shows the playhead's spot (paused): catches a picture stuck at an old time.
function Check-Picture($s, [string]$label) {
  $r = Settle $s 250
  # A file can hold its last frame well past the frame's start: the picture stops there.
  $want = if ($null -ne $r.picture_end) { [math]::Min([double]$r.pos, [double]$r.picture_end) } else { [double]$r.pos }
  $ok = [math]::Abs([double]$r.video - $want) -lt 0.05
  [pscustomobject]@{ Pass = $ok; Detail = "$label playhead $(Fmt $r.pos)s, picture $(Fmt $r.video)s" }
}
# Derives a test clip with ffmpeg on the background cores (once; reused after).
function Invoke-Ffmpeg([string[]]$arguments) {
  if (-not $script:ffmpeg) { throw 'ffmpeg.exe is not on PATH (needed once to derive the test clips)' }
  $quoted = (@('-v', 'error', '-y') + $arguments) | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }
  $p = Start-Process -FilePath $script:ffmpeg -ArgumentList $quoted -NoNewWindow -PassThru
  [void]$p.Handle # keeps the exit code readable after WaitForExit
  try { $p.ProcessorAffinity = [IntPtr][int64]4294901760; $p.PriorityClass = 'BelowNormal' } catch { }
  $p.WaitForExit()
  if ($p.ExitCode) { throw "ffmpeg failed: $($arguments -join ' ')" }
}
# Waits until playback stops by itself; returns the trace line (or $null).
function Wait-Stopped($s, [int]$timeoutMs) {
  $since = (Last $s).ms
  Wait-Trace $s { param($r) -not $r.playing -and $r.ms -gt $since + 60 } $timeoutMs
}

function Add-Result([string]$name, [bool]$pass, [string]$detail) {
  $script:results += [pscustomobject]@{ Scenario = $name; Result = $(if ($pass) { 'PASS' } else { 'FAIL' }); Detail = $detail }
  Write-Host ("{0,-4} {1} :: {2}" -f $(if ($pass) { 'PASS' } else { 'FAIL' }), $name, $detail)
}
# Runs one scenario block; a thrown error is a FAIL row, never an aborted run.
function Run-Scenario([string]$name, [scriptblock]$body) {
  if ($script:only -and -not ($script:only | Where-Object { $name -like $_ })) { return }
  $script:current = $null; $script:scenario = $name
  try { & $body } catch { Add-Result $name $false "error: $($_.Exception.Message)" }
  finally { if ($script:current) { Stop-Viewer $script:current; $script:current = $null } }
}
function Ok([bool]$pass, [string]$detail) { [pscustomobject]@{ Pass = $pass; Detail = $detail } }
# One result row for the running scenario from its checks (all must pass).
function Verdict([object[]]$checks) {
  $pass = -not ($checks | Where-Object { -not $_.Pass })
  Add-Result $script:scenario ([bool]$pass) (($checks | ForEach-Object { $(if ($_.Pass) { '' } else { 'FAILED ' }) + $_.Detail }) -join ' | ')
}
function Fmt([double]$v) { '{0:N2}' -f $v }
