<#
  Progressive opening on a slow drive (dev check; run after changes to the clip fill, the player's
  byte stream, live sound or the loading states).

  Each scenario opens a fresh hard link of a test clip in a private, offscreen, silent Recall with
  RECALL_SLOW_DRIVE=<MB/s>, which makes the clip's drive act like a hard drive of that speed, and an
  empty cache, so nothing is prepared yet. Drives the app like a person and reads its trace.

    powershell -NoProfile -File scripts\dev\slow-drive-check.ps1 [-Clip <mp4>] [-Exe <recall-native.exe>] [-Only '<pattern>', ...]

  -Clip: at least 60 s and 100 MB; default .tmp\viewer-check\base.mp4.
  Needs an engine that knows --prepare-editor: RECALL_ENGINE, else target-dev\debug\recall.exe under
  -EngineRoot. An older engine does not fail: it starts a recorder and never returns.
  -RealClip <path>, ...: also opens each clip where it lies, unslowed and unlinked (read only).
  -Snap <png>: saves the window from that scenario while it loads.
#>
param([string]$Clip = '', [string]$Exe = '', [string[]]$Only = @(), [string]$EngineRoot = 'L:\General\recall', [string[]]$RealClip = @(), [string]$Snap = '')
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
. (Join-Path $PSScriptRoot 'viewer-check-lib.ps1')
Initialize-ViewerCheck $repo $Exe
$script:only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
Write-Host "player: $script:exe"
if (-not $Clip) { $Clip = Join-Path $script:work 'base.mp4' }
if (-not $env:RECALL_ENGINE) { $env:RECALL_ENGINE = Join-Path $EngineRoot 'target-dev\debug\recall.exe' }
if (-not (Select-String -Path $env:RECALL_ENGINE -Pattern '--prepare-editor' -SimpleMatch -Quiet)) { throw "$env:RECALL_ENGINE has no --prepare-editor: it would start a recorder" }
$size = (Get-Item $Clip).Length

function Start-Slow([string]$name, [int]$rate, [switch]$Library, [string[]]$Also = @()) {
  foreach ($tier in 'video', 'session', 'editor', 'visuals', 'tmp') { Remove-Item (Join-Path $script:cache $tier) -Recurse -Force -ErrorAction SilentlyContinue }
  $env:RECALL_SLOW_DRIVE = "$rate"
  try { $s = if ($Library) { Start-Viewer $name $Clip -Library -Also $Also } else { Start-Viewer $name $Clip } }
  finally { Remove-Item Env:RECALL_SLOW_DRIVE -ErrorAction SilentlyContinue }
  $script:current = $s
  $s
}
function Fills { @(Get-ChildItem (Join-Path $script:cache 'video'), (Join-Path $script:cache 'session') -Filter '*.part' -ErrorAction SilentlyContinue) }
function Parts($r) { if ($r.loading) { [int]$r.loading.grey_parts } else { 0 } }

Run-Scenario 'the picture and sound start while the clip loads' {
  $seconds = [int]($size / 1MB / 20)
  $s = Start-Slow 'slow-start' 20
  $first = Last $s
  $open = Ok ($s.OpenMs -lt 4000 -and $null -ne $first.loading) "ready in $($s.OpenMs) ms of a ${seconds} s copy, loaded $(Fmt ($first.loading.share * 100))%"
  $play = Press-Play $s 'space' 3
  $r = Last $s
  $more = Ok ($r.loading.share -gt $first.loading.share) "still loading while playing: $(Fmt ($first.loading.share * 100))% -> $(Fmt ($r.loading.share * 100))%"
  $grey = Ok ((Parts $r) -ge 1) "grey parts on the timeline: $(Parts $r)"
  $wave = Ok ($r.loading.waves -ge 1) "live waveforms: $($r.loading.waves)"
  $none = Ok (-not $r.status) "status '$($r.status)'"
  Verdict @($open, $play, $more, $grey, $wave, $none)
}

Run-Scenario 'the grey shrinks to nothing and the engine media arrive' {
  $s = Start-Slow 'slow-finish' 60
  $widths = @()
  $done = $null
  $t = Get-Date
  while (((Get-Date) - $t).TotalSeconds -lt 120) {
    $r = Last $s
    if ($r.loading) { $widths += [double]$r.loading.grey_seconds }
    if (-not $r.loading -and $r.stems -and $r.stems[0]) { $done = $r; break }
    Start-Sleep -Milliseconds 300
  }
  $shrank = Ok ($widths.Count -gt 3 -and $widths[-1] -lt $widths[0]) "grey seconds $(Fmt $widths[0]) -> $(Fmt $widths[-1]) in $($widths.Count) samples"
  $finished = Ok ($null -ne $done) "stems prepared, loading state gone after $(Fmt ((Get-Date) - $t).TotalSeconds) s"
  $copy = Ok (@(Get-ChildItem (Join-Path $script:cache 'video') -Filter '*.mp4').Count -eq 1) 'the copy landed in the video tier'
  $parts = Ok ((Fills).Count -eq 0) 'no part file left'
  $plays = Press-Play $s 'space' 1.5
  Verdict @($shrank, $finished, $copy, $parts, $plays)
}

Run-Scenario 'a seek ahead of the copy pulls it there and plays' {
  $s = Start-Slow 'slow-seek' 15
  $target = [double](Last $s).duration * 0.8
  $t0 = (Last $s).ms
  Scrub $s 0.1 $target 6
  $landed = Wait-Trace $s { param($r) $r.loading -and -not $r.loading.note -and [math]::Abs([double]$r.video - [double]$r.pos) -lt 0.1 -and [double]$r.pos -gt 1 } 6000
  $at = Ok ($null -ne $landed) $(if ($landed) { "picture at $(Fmt $landed.video)s after $([int]($landed.ms - $t0)) ms" } else { 'picture never reached the seek' })
  $notes = @(Rows $s $t0 | Where-Object { $_.loading.note }).Count
  $saw = Ok ($notes -gt 0) "said it was loading that part in $notes samples"
  $plays = Press-Play $s 'space' 2
  $r = Last $s
  $two = Ok ((Parts $r) -ge 2) "grey parts before and after the jump: $(Parts $r)"
  Verdict @($at, $saw, $plays, $two)
}

Run-Scenario 'playback pauses for a drive slower than the clip and carries on' {
  $bitrate = $size / [double](ffprobe -v error -show_entries format=duration -of csv=p=0 $Clip) / 1MB
  $s = Start-Slow 'slow-buffer' ([math]::Max(1, [int]($bitrate * 0.6)))
  $t0 = (Settle $s 60).ms
  Key $s 'space'
  $stopped = Wait-Trace $s { param($r) $r.loading.buffering } 40000
  $wait = Ok ($null -ne $stopped) $(if ($stopped) { "said Buffering at $(Fmt $stopped.pos)s (clip $(Fmt $bitrate) MB/s, drive slower)" } else { 'never buffered' })
  $again = if ($stopped) { Wait-Trace $s { param($r) -not $r.loading.buffering -and $r.video_state -eq 'playing' -and $r.ms -gt $stopped.ms } 20000 }
  $resumed = Ok ($null -ne $again) 'carried on by itself'
  $r = Last $s
  $ok = Ok (-not $r.status) "status '$($r.status)', stalls gave up: $(@(Rows $s $t0 | Where-Object { $_.status -like 'Playback stopped*' }).Count)"
  Verdict @($wait, $resumed, $ok)
}

Run-Scenario 'edits made while loading survive the upgrade' {
  $s = Start-Slow 'slow-edit' 60
  Scrub $s 0.1 3 4
  $at = (Settle $s 300).pos
  Key $s 'c'
  $cut = Settle $s
  $split = Ok (@($cut.ranges).Count -eq 2) "split at $(Fmt $at)s while loading: $(@($cut.ranges).Count) ranges"
  $done = Wait-Trace $s { param($r) -not $r.loading -and $r.stems -and $r.stems[0] } 60000
  $kept = Ok ($done -and @($done.ranges).Count -eq 2 -and $done.undo) 'the split and its undo survive'
  Verdict @($split, $kept)
}

Run-Scenario 'leaving mid-load stops the copy cleanly' {
  $s = Start-Slow 'slow-leave' 10 -Library -Also @('slow-other')
  Click $s 'tile:slow-leave.mp4'
  [void](Wait-Trace $s { param($r) $r.ready -and $r.loading } 8000)
  $one = Fills
  Key $s 'escape'; Start-Sleep -Milliseconds 400
  $r = Last $s
  $started = Ok ($one.Count -eq 1) "copying: $($one.Count) part file"
  $t = Get-Date
  Stop-Viewer $s; $script:current = $null
  $closed = Ok ($s.Process.HasExited -and $s.Process.ExitCode -eq 0 -and ((Get-Date) - $t).TotalSeconds -lt 5) "closed mid-load in $(Fmt ((Get-Date) - $t).TotalSeconds) s, exit $($s.Process.ExitCode)"
  # A part file a closed app left is replaced by the next open of that clip.
  Start-Sleep -Seconds 3 # thumbnail helpers let go of the link
  $env:RECALL_SLOW_DRIVE = '10'
  try { $again = Start-Viewer 'slow-leave' $Clip -Library -Also @('slow-other') } finally { Remove-Item Env:RECALL_SLOW_DRIVE -ErrorAction SilentlyContinue }
  $script:current = $again
  Click $again 'tile:slow-leave.mp4'
  [void](Wait-Trace $again { param($r) $r.ready -and $r.loading } 8000)
  $once = Ok ((Fills).Count -eq 1) "after reopening: $((Fills).Count) part file"
  Verdict @($started, $closed, $once)
}

Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing @'
using System; using System.Drawing; using System.Runtime.InteropServices;
public static class Shot {
  [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
  [StructLayout(LayoutKind.Sequential)] struct R { public int L, T, X, B; }
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out R r);
  public static void Save(IntPtr h, string path) {
    SetProcessDPIAware(); R r; GetWindowRect(h, out r);
    using (var b = new Bitmap(2000, 1300)) using (var g = Graphics.FromImage(b)) {
      IntPtr dc = g.GetHdc(); PrintWindow(h, dc, 2); g.ReleaseHdc(dc); b.Save(path);
    }
  }
}
'@
# PrintWindow breaks the running player's repaint: nothing else may follow it in this instance.
if ($Snap) {
  Run-Scenario 'snapshot while loading' {
    $script:windowSize = '1600x1100'
    $s = Start-Slow 'slow-snap' 15
    $script:windowSize = $null
    Start-Sleep -Seconds 3
    [Shot]::Save($s.Hwnd, $Snap)
    Add-Result $script:scenario (Test-Path $Snap) $Snap
  }
}
$real = @($RealClip | ForEach-Object { $_ -split '\|' } | Where-Object { $_ })
foreach ($path in $real) {
  Run-Scenario "real drive: $(Split-Path $path -Leaf)" {
    # Where it lies: no link, so nothing on that drive is touched.
    function New-ClipLink([string]$dir, [string]$name, [string]$from) { $from }
    foreach ($tier in 'video', 'session', 'editor', 'visuals', 'tmp') { Remove-Item (Join-Path $script:cache $tier) -Recurse -Force -ErrorAction SilentlyContinue }
    $name = 'real-' + [IO.Path]::GetFileNameWithoutExtension($path).Substring(0, 8)
    $s = Start-Viewer $name $path
    $script:current = $s
    $first = Last $s
    $open = Ok ($true) "ready in $($s.OpenMs) ms, loaded $(if ($first.loading) { Fmt ($first.loading.share * 100) } else { 'all' })%, status '$($first.status)'"
    $checks = @($open)
    if ($first.loading) {
      $checks += Press-Play $s 'space' 3
      Key $s 'space'
      $t = Get-Date
      $done = Wait-Trace $s { param($r) -not $r.loading } 120000
      $checks += Ok ($null -ne $done) "whole clip plus stems after $(Fmt ((Get-Date) - $t).TotalSeconds) s more"
    }
    Verdict $checks
  }
}

$fails = @($script:results | Where-Object { $_.Result -eq 'FAIL' }).Count
Write-Host "$($script:results.Count - $fails)/$($script:results.Count) passed"
exit $(if ($fails) { 1 } else { 0 })
