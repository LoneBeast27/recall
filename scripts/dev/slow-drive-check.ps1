<#
  Progressive opening on a slow drive (dev check; run after changes to the clip fill, the player's
  byte stream, live sound or the loading states).

  Each scenario opens a fresh hard link of a test clip in a private, offscreen, silent Recall with
  RECALL_SLOW_DRIVE=<MB/s>, which makes the clip's drive act like a hard drive of that speed, and an
  empty cache, so nothing is prepared yet. Drives the app like a person and reads its trace.

    powershell -NoProfile -File scripts\dev\slow-drive-check.ps1 [-Clip <mp4>] [-Exe <recall-native.exe>] [-Only '<pattern>', ...]

  -Clip: at least 60 s and 100 MB; default .tmp\viewer-check\base.mp4.
#>
param([string]$Clip = '', [string]$Exe = '', [string[]]$Only = @())
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
. (Join-Path $PSScriptRoot 'viewer-check-lib.ps1')
Initialize-ViewerCheck $repo $Exe
$script:only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
Write-Host "player: $script:exe"
if (-not $Clip) { $Clip = Join-Path $script:work 'base.mp4' }
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
function Grey($r) { if ($r.loading) { @($r.loading.grey) } else { @() } }

Run-Scenario 'the picture and sound start while the clip loads' {
  $seconds = [int]($size / 1MB / 20)
  $s = Start-Slow 'slow-start' 20
  $first = Last $s
  $open = Ok ($s.OpenMs -lt 4000 -and $null -ne $first.loading) "ready in $($s.OpenMs) ms of a ${seconds} s copy, loaded $(Fmt ($first.loading.share * 100))%"
  $play = Press-Play $s 'space' 3
  $r = Last $s
  $more = Ok ($r.loading.share -gt $first.loading.share) "still loading while playing: $(Fmt ($first.loading.share * 100))% -> $(Fmt ($r.loading.share * 100))%"
  $grey = Ok ((Grey $r).Count -ge 1) "grey parts on the timeline: $((Grey $r).Count)"
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
    if ($r.loading) { $widths += [double](((Grey $r) | ForEach-Object { [double]$_[1] - [double]$_[0] } | Measure-Object -Sum).Sum) }
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
  $two = Ok ((Grey $r).Count -ge 2) "grey parts before and after the jump: $((Grey $r).Count)"
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
  Key $s 'right' 3
  $at = (Settle $s).pos
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
  Stop-Viewer $s; $script:current = $null
  Start-Sleep -Milliseconds 300
  $gone = Ok ($s.Process.HasExited -and (Fills).Count -eq 0) "closed, part files left: $((Fills).Count), exit $($s.Process.ExitCode)"
  Verdict @($started, $gone)
}

$fails = @($script:results | Where-Object { $_.Result -eq 'FAIL' }).Count
Write-Host "$($script:results.Count - $fails)/$($script:results.Count) passed"
exit $(if ($fails) { 1 } else { 0 })
