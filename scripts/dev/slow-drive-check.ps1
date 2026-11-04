<#
  Progressive opening on a slow drive (dev check; run after changes to the clip fill, the player's
  byte stream, live sound or the loading states).

  Each scenario opens a fresh hard link of a test clip in a private, offscreen, silent Recall with
  RECALL_SLOW_DRIVE=<MB/s>, which makes the clip's drive act like a hard drive of that speed, and an
  empty cache, so nothing is prepared yet. Drives the app like a person and reads its trace.

    powershell -NoProfile -File scripts\dev\slow-drive-check.ps1 [-Clip <mp4>] [-Exe <recall-native.exe>] [-Only '<pattern>', ...]

  -Clip: at least 60 s and 100 MB; default .tmp\viewer-check\base.mp4.
  Needs an engine stamped recall-engine-caps: RECALL_ENGINE, else target-dev\debug\recall.exe under
  -EngineRoot. An older engine does not fail: it starts a recorder and never returns.
  The engine scenarios run stand-ins built here (slow-drive-stub.ps1), never a real older build.
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
if (-not (Select-String -Path $env:RECALL_ENGINE -Pattern 'recall-engine-caps:' -SimpleMatch -Quiet)) { throw "$env:RECALL_ENGINE has no caps stamp: the app refuses it" }
. (Join-Path $PSScriptRoot 'slow-drive-stub.ps1')
$script:realEngine = $env:RECALL_ENGINE
$size = (Get-Item $Clip).Length

function Start-Slow([string]$name, [int]$rate, [switch]$Library, [string[]]$Also = @()) {
  foreach ($tier in 'video', 'editor', 'visuals', 'tmp') { Remove-Item (Join-Path $script:cache $tier) -Recurse -Force -ErrorAction SilentlyContinue }
  $env:RECALL_SLOW_DRIVE = "$rate"
  try { $s = if ($Library) { Start-Viewer $name $Clip -Library -Also $Also } else { Start-Viewer $name $Clip } }
  finally { Remove-Item Env:RECALL_SLOW_DRIVE -ErrorAction SilentlyContinue }
  $script:current = $s
  $s
}
function Fills { @(Get-ChildItem (Join-Path $script:cache 'video') -Filter '*.part' -ErrorAction SilentlyContinue) }
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
  # Play once the 3 s after the playhead are on the SSD. Pressed at once, playback measures how
  # fast the slowed copy gets ahead of it, not whether it plays: under CPU or disk load the copy
  # trails and the 2 s window spent most of its time buffering (+0.64 s in a full gate run).
  $ahead = Wait-Trace $s { param($r) $p = [double]$r.pos; $r.loading -and -not @($r.loading.grey | Where-Object { [double]$_[0] -lt $p + 3 -and [double]$_[1] -gt $p }) } 30000
  $loaded = Ok ($null -ne $ahead) $(if ($ahead) { "3 s past the playhead loaded after $([int]($ahead.ms - $t0)) ms" } else { 'the part after the playhead never loaded' })
  $plays = Press-Play $s 'space' 2
  $r = Last $s
  $two = Ok ((Parts $r) -ge 2) "grey parts before and after the jump: $(Parts $r)"
  Verdict @($at, $saw, $loaded, $plays, $two)
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
  $started = Ok ($one.Count -eq 1) "copying: $($one.Count) part file"
  Key $s 'escape'
  [void](Wait-Trace $s { param($r) $r.route -eq 'library' } 3000)
  Start-Sleep -Milliseconds 500
  # Away from the editor the copy carries on, paced to at most PACED_RATE (fill.rs, 40 MB/s).
  $a = (Last $s).loading.share; Start-Sleep -Seconds 2; $b = (Last $s).loading.share
  $rate = ($b - $a) * $size / 2
  $held = Ok ($b -gt $a -and $rate -le 44e6) "away from the editor the copy carries on, paced: $(Fmt ($a * 100))% -> $(Fmt ($b * 100))%, $(Fmt ($rate / 1e6)) MB/s"
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
  Verdict @($started, $held, $closed, $once)
}

Run-Scenario 'opening another clip stops the first copy' {
  $s = Start-Slow 'slow-first' 20 -Library -Also @('slow-second')
  Click $s 'tile:slow-first.mp4'
  [void](Wait-Trace $s { param($r) $r.ready -and $r.loading } 8000)
  $a = @(Fills)
  $one = Ok ($a.Count -eq 1) "the first clip is copying: $($a.Count) part file"
  Key $s 'escape'
  [void](Wait-Trace $s { param($r) $r.route -eq 'library' } 3000)
  Click $s 'tile:slow-second.mp4'
  $t = Get-Date
  while ((Test-Path -LiteralPath $a[0].FullName) -and ((Get-Date) - $t).TotalSeconds -lt 5) { Start-Sleep -Milliseconds 20 }
  $gone = Ok (-not (Test-Path -LiteralPath $a[0].FullName)) "its part file went $([int]((Get-Date) - $t).TotalMilliseconds) ms after the second open"
  [void](Wait-Trace $s { param($r) $r.ready -and $r.loading -and $r.clip -like '*slow-second*' } 8000)
  $b = @(Fills)
  $other = Ok ($b.Count -eq 1 -and $b[0].Name -ne $a[0].Name) "the second clip copies: $($b.Count) part file, another name"
  $t = Get-Date
  while ((@(Get-ChildItem (Join-Path $script:cache 'video') -Filter '*.mp4').Count -eq 0) -and ((Get-Date) - $t).TotalSeconds -lt 40) { Start-Sleep -Milliseconds 200 }
  Start-Sleep -Milliseconds 300
  $landed = @(Get-ChildItem (Join-Path $script:cache 'video') -Filter '*.mp4')
  $only = Ok ($landed.Count -eq 1 -and (Fills).Count -eq 0) "the second copy finished whole: $($landed.Count) copy, $((Fills).Count) part files"
  Verdict @($one, $gone, $other, $only)
}

Run-Scenario 'a second clip that cannot open still stops the first copy' {
  $s = Start-Slow 'slow-before-bad' 10 -Library
  $bad = Join-Path (Split-Path $s.Clip) 'slow-bad.mp4'
  [IO.File]::WriteAllBytes($bad, [byte[]]::new(2MB))
  [void](Wait-Trace $s { param($r) $r.targets.PSObject.Properties.Name -contains 'tile:slow-bad.mp4' } 10000)
  Click $s 'tile:slow-before-bad.mp4'
  [void](Wait-Trace $s { param($r) $r.ready -and $r.loading } 8000)
  $a = @(Fills)
  Key $s 'escape'
  [void](Wait-Trace $s { param($r) $r.route -eq 'library' } 3000)
  Click $s 'tile:slow-bad.mp4'
  $failed = Wait-Trace $s { param($r) -not $r.clip -and $r.status -and $r.status -notlike 'Opening*' -and $r.status -notlike 'Preparing*' -and $r.status -notlike 'Loading*' } 15000
  $t = Get-Date
  while ((Test-Path -LiteralPath $a[0].FullName) -and ((Get-Date) - $t).TotalSeconds -lt 3) { Start-Sleep -Milliseconds 20 }
  $state = if (Test-Path -LiteralPath $a[0].FullName) { 'still running' } else { 'stopped' }
  Verdict @((Ok ($a.Count -eq 1 -and $state -eq 'stopped') "first copy after the failed open: $state"), (Ok ($null -ne $failed) "said: $($failed.status)"))
}

Run-Scenario 'edits made during an export survive reopening the clip' {
  $stub = New-StubEngine 'stub-new' -Caps
  $env:RECALL_ENGINE = $stub; $env:RECALL_STUB_REAL = $script:realEngine; $env:RECALL_STUB_EXPORT_MS = '25000'; $env:RECALL_STUB_LOG = Join-Path $script:work 'stub-new.log'
  Remove-Item $env:RECALL_STUB_LOG -ErrorAction SilentlyContinue
  try {
    $s = Start-Slow 'slow-keep' 200 -Library -Also @('slow-away')
    Click $s 'tile:slow-keep.mp4'
    [void](Wait-Trace $s { param($r) $r.ready -and $r.clip -like '*slow-keep*' -and $r.targets.tracks } 8000)
    Scrub $s 0.1 3 4; [void](Settle $s 300); Key $s 'c'
    [void](Wait-Trace $s { param($r) @($r.ranges).Count -eq 2 -and $r.saved } 4000)
    Click $s 'export'
    $running = Wait-StubLog '--export-edit' 5000
    Scrub $s 0.1 6 4; [void](Settle $s 300); Key $s 'c'
    $three = Wait-Trace $s { param($r) @($r.ranges).Count -eq 3 } 3000
    Key $s 'escape'
    [void](Wait-Trace $s { param($r) $r.route -eq 'library' } 3000)
    Click $s 'tile:slow-away.mp4'
    [void](Wait-Trace $s { param($r) $r.ready -and $r.clip -like '*slow-away*' } 8000)
    Key $s 'escape'
    [void](Wait-Trace $s { param($r) $r.route -eq 'library' } 3000)
    Click $s 'tile:slow-keep.mp4'
    $back = Wait-Trace $s { param($r) $r.ready -and $r.clip -like '*slow-keep*' -and $r.targets.tracks } 8000
    $still = $null -ne $running -and -not $running.HasExited
    $during = Ok ($still -and $three -and @($back.ranges).Count -eq 3) "reopened with the export still running ($still): $(@($back.ranges).Count) ranges of 3"
    if ($running) { [void]$running.WaitForExit(30000) }
    $sidecar = "$($s.Clip).recall-edit.json"
    $t = Get-Date; $beside = 0
    while ($beside -ne 3 -and ((Get-Date) - $t).TotalSeconds -lt 5) {
      if (Test-Path -LiteralPath $sidecar) { $beside = @((Get-Content -LiteralPath $sidecar -Raw | ConvertFrom-Json).plan.ranges).Count }
      Start-Sleep -Milliseconds 100
    }
    Verdict @($during, (Ok ($beside -eq 3) "after the export the edit file beside the clip has $beside ranges"))
  } finally { Stop-Stubs 'stub-new'; Reset-Engine }
}

function Over-Cap([scriptblock]$body) {
  # The budget decides, not the clip: half of it is under this clip's size.
  Set-Content -LiteralPath (Join-Path $script:cache 'storage.json') -Value "{`"cache_budget_bytes`":$size}"
  try { & $body } finally { Remove-Item (Join-Path $script:cache 'storage.json') -ErrorAction SilentlyContinue }
}

Run-Scenario 'a clip over the tier cap streams from the drive and is not copied' {
  Over-Cap {
    $s = Start-Slow 'slow-big' 20
    $first = Last $s
    $open = Ok ($s.OpenMs -lt 4000 -and $null -ne $first.loading) "cap $(Fmt ($size / 2MB)) MB, clip $(Fmt ($size / 1MB)) MB: ready in $($s.OpenMs) ms, loading state on"
    $play = Press-Play $s 'space' 2
    Key $s 'space'
    $target = [double](Last $s).duration * 0.8
    Scrub $s 0.1 $target 6
    $landed = Wait-Trace $s { param($r) [math]::Abs([double]$r.video - [double]$r.pos) -lt 0.1 -and [double]$r.pos -gt ($target - 2) } 6000
    $seek = Ok ($null -ne $landed) $(if ($landed) { "picture at $(Fmt $landed.video)s with no copy behind it" } else { 'picture never reached the seek' })
    $stems = Wait-Trace $s { param($r) $r.stems -and $r.stems[0] } 90000
    $got = Ok ($null -ne $stems) 'stems prepared from the clip where it lies'
    $copied = @(Get-ChildItem (Join-Path $script:cache 'video') -File -ErrorAction SilentlyContinue)
    $tier = [long](($copied | Measure-Object Length -Sum).Sum)
    $clean = Ok (@($copied | Where-Object { $_.Extension -in '.mp4', '.part' }).Count -eq 0 -and $tier -lt 1MB) "video tier holds $($copied.Count) files, $tier bytes"
    Verdict @($open, $play, $seek, $got, $clean)
  }
}

Run-Scenario 'a clip over the cap whose drive goes away says so' {
  Over-Cap {
    $env:RECALL_SLOW_DRIVE_FAIL = '60'
    try { $s = Start-Slow 'slow-big-gone' 20 } finally { Remove-Item Env:RECALL_SLOW_DRIVE_FAIL -ErrorAction SilentlyContinue }
    Scrub $s 0.1 ([double](Last $s).duration * 0.6) 6
    $failed = Wait-Trace $s { param($r) $r.loading.failed } 10000
    Key $s 'space'
    $stopped = Wait-Trace $s { param($r) -not $r.playing } 8000
    $r = Last $s
    Verdict @((Ok ($null -ne $failed) "said: $($failed.loading.failed)"), (Ok ($null -ne $stopped -and -not $r.loading.buffering -and -not $s.Process.HasExited) "stopped, not buffering, alive; note '$($r.loading.note)'"))
  }
}

Run-Scenario 'sound that starts after the picture does not hang' {
  $late = Join-Path $script:work 'late-audio.mp4'
  if (-not (Test-Path $late)) { Invoke-Ffmpeg @('-t', '20', '-i', $Clip, '-itsoffset', '3', '-t', '20', '-i', $Clip, '-map', '0:v', '-map', '1:a:0', '-c', 'copy', $late) }
  $whole = $Clip
  try {
    $Clip = $late
    $s = Start-Slow 'slow-late' 20
    $play = Press-Play $s 'space' 2
    $r = Last $s
    $free = Ok (-not $r.loading.buffering -and [double]$r.pos -gt 1) "not buffering at $(Fmt $r.pos)s"
    # The first 0.4 s after a buffered start can show the picture up to 0.4 s ahead of the sound clock.
    $start = (Rows $s 0 | Where-Object { $_.playing -and -not $_.loading.buffering -and $_.video_state -eq 'playing' } | Select-Object -First 1).ms
    $steady = @(Rows $s ($start + 500) | Where-Object { $_.playing -and -not $_.loading.buffering })
    $gap = [double](($steady | ForEach-Object { [math]::Abs([double]$_.video - [double]$_.pos) } | Measure-Object -Maximum).Maximum)
    $live = Ok ($steady.Count -gt 10 -and $gap -lt 0.1) "plays from 0 over the silent start; steady A/V gap $(Fmt $gap)s; $($play.Detail)"
    $starts = [double](& ffprobe -v error -select_streams a:0 -show_entries stream=start_time -of csv=p=0 $late)
    $offset = Ok ($starts -ge 2.9) "the file's sound starts at $(Fmt $starts)s"
    Key $s 'space'
    $done = Wait-Trace $s { param($r) -not $r.loading -and $r.stems -and $r.stems[0] } 60000
    $loaded = Ok ($null -ne $done) 'finished loading, stems in'
    # What a decoder stuck on the silent head costs: one core, for as long as the clip is open.
    $before = $s.Process.TotalProcessorTime; $t = Get-Date; Start-Sleep -Seconds 4; $s.Process.Refresh()
    $cores = ($s.Process.TotalProcessorTime - $before).TotalSeconds / ((Get-Date) - $t).TotalSeconds
    Verdict @($offset, $live, $free, $loaded, (Ok ($cores -lt 0.5) "idle after loading: $(Fmt $cores) cores"))
  } finally { $Clip = $whole }
}

Run-Scenario 'an engine without the prepare command is refused, not started' {
  $stub = New-StubEngine 'stub-old'
  $env:RECALL_ENGINE = $stub; $env:RECALL_STUB_LOG = Join-Path $script:work 'stub-old.log'
  Remove-Item $env:RECALL_STUB_LOG -ErrorAction SilentlyContinue
  try {
    $s = Start-Slow 'slow-old-engine' 200
    $settled = Wait-Trace $s { param($r) -not $r.loading } 15000
    $plays = Press-Play $s 'space' 1
    $runs = if (Test-Path $env:RECALL_STUB_LOG) { @(Get-Content $env:RECALL_STUB_LOG).Count } else { 0 }
    Verdict @((Ok ($runs -eq 0) "a stand-in engine with no stamp: started $runs times"), (Ok ($null -ne $settled) 'the load ended without it'), $plays)
  } finally { Stop-Stubs 'stub-old'; Reset-Engine }
}

Run-Scenario 'an engine that ignores the prepare command is given up on and killed' {
  $short = Join-Path $script:work 'short-engine.mp4'
  if (-not (Test-Path $short)) { Invoke-Ffmpeg @('-t', '6', '-i', $Clip, '-map', '0', '-c', 'copy', $short) }
  $Clip = $short
  $stub = New-StubEngine 'stub-hang' -Caps
  $env:RECALL_ENGINE = $stub; $env:RECALL_STUB_LOG = Join-Path $script:work 'stub-hang.log'
  Remove-Item $env:RECALL_STUB_LOG -ErrorAction SilentlyContinue
  try {
    $limit = 120 + (Get-Item $short).Length / 2e6
    $s = Start-Slow 'slow-hung-engine' 200
    $hung = Wait-StubLog '--prepare-editor' 15000
    $t = Get-Date
    $started = Ok ($null -ne $hung -and -not $hung.HasExited) "a stand-in with the stamp ignores the command and stays: pid $($hung.Id)"
    $end = $hung.WaitForExit([int](($limit + 15) * 1000))
    $took = ((Get-Date) - $t).TotalSeconds
    $killed = Ok ($end -and $took -lt $limit + 10) "killed after $(Fmt $took) s (limit $(Fmt $limit) s)"
    $r = Wait-Trace $s { param($r) -not $r.loading } 5000
    $plays = Press-Play $s 'space' 1
    Stop-Viewer $s; $script:current = $null
    Remove-Item $env:RECALL_STUB_LOG -ErrorAction SilentlyContinue
    # Nobody may be left with a recorder when the window closes either.
    $s = Start-Slow 'slow-hung-engine' 200
    $again = Wait-StubLog '--prepare-editor' 15000
    Stop-Viewer $s; $script:current = $null
    $orphan = $null -eq $again -or -not $again.WaitForExit(3000)
    $state = if ($orphan) { 'left running' } else { 'gone' }
    Verdict @($started, $killed, (Ok ($null -ne $r) 'the load ended without it'), $plays, (Ok (-not $orphan) "window closed with the engine running: engine $state"))
  } finally { Stop-Stubs 'stub-hang'; Reset-Engine }
}

Run-Scenario 'a drive that goes away leaves an error and what loaded' {
  $env:RECALL_SLOW_DRIVE_FAIL = '60'
  try { $s = Start-Slow 'slow-gone' 30 } finally { Remove-Item Env:RECALL_SLOW_DRIVE_FAIL -ErrorAction SilentlyContinue }
  $failed = Wait-Trace $s { param($r) $r.loading.failed } 10000
  $said = Ok ($null -ne $failed) "said: $($failed.loading.failed)"
  $t0 = (Settle $s 60).ms
  Key $s 'space'
  Start-Sleep -Seconds 1
  $r = Wait-Trace $s { param($r) -not $r.playing } 30000
  $stopped = Ok ($null -ne $r -and -not $r.loading.buffering -and [double]$r.pos -gt 5 -and -not $r.status) "what loaded played to $(Fmt $r.pos)s, note '$($r.loading.note)', status '$($r.status)'"
  $never = @(Rows $s $t0 | Where-Object { $_.loading.buffering }).Count
  $end = [double](Last $s).duration
  Scrub $s 0.1 ($end * 0.8) 6
  $hole = Wait-Trace $s { param($r) $r.loading.note -eq "This part didn't load" } 4000
  Scrub $s 0.1 3 6
  $back = Wait-Trace $s { param($r) [double]$r.pos -lt 4 -and [math]::Abs([double]$r.video - [double]$r.pos) -lt 0.15 } 5000
  $seek = Ok ($null -ne $hole -and $null -ne $back) "a seek into what did not load, then back: picture at $(if ($back) { Fmt $back.video } else { 'nowhere' }) s, note was '$($hole.loading.note)'"
  Verdict @($said, $stopped, (Ok ($never -eq 0) "buffering samples: $never"), $seek)
}

Run-Scenario 'a drive that goes away during a wait ends the wait' {
  $env:RECALL_SLOW_DRIVE_FAIL = '20'
  try { $s = Start-Slow 'slow-gone-wait' 1 } finally { Remove-Item Env:RECALL_SLOW_DRIVE_FAIL -ErrorAction SilentlyContinue }
  Key $s 'space'
  $waiting = Wait-Trace $s { param($r) $r.loading.buffering } 15000
  $failed = Wait-Trace $s { param($r) $r.loading.failed } 40000
  $free = if ($failed) { Wait-Trace $s { param($r) -not $r.loading.buffering -and $r.ms -gt $failed.ms } 3000 }
  Start-Sleep -Milliseconds 500
  $r = Last $s
  $before = if ($failed) { @(Rows $s 0 | Where-Object { $_.ms -lt $failed.ms }) | Select-Object -Last 1 }
  Verdict @((Ok ($null -ne $waiting -and $null -ne $failed -and $before.loading.buffering) "buffered up to the failure: $($before.loading.buffering); $($failed.loading.failed)"), (Ok ($null -ne $free -and -not $r.loading.buffering) "Buffering ended by itself: playing=$($r.playing), note '$($r.loading.note)'"))
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
# The engine reads a big clip where it lies while it plays: on a real drive the lane stays free.
$big = $real | Where-Object { (Get-Item -LiteralPath $_).Length -gt 1GB } | Select-Object -First 1
if ($big) {
  Run-Scenario 'real drive: the engine leaves the lane free beside a big clip' {
    function New-ClipLink([string]$dir, [string]$name, [string]$from) { $from }
    foreach ($tier in 'video', 'editor', 'visuals', 'tmp') { Remove-Item (Join-Path $script:cache $tier) -Recurse -Force -ErrorAction SilentlyContinue }
    $s = Start-Viewer 'real-big' $big
    $script:current = $s
    $work = [IO.Path]::GetFullPath($script:work)
    $t = Get-Date; $parts = 0; $from = $null; $to = $null
    while (((Get-Date) - $t).TotalSeconds -lt 90) {
      $r = Last $s
      if ($r.stems -and $r.stems[0]) { break }
      if (@(Get-CimInstance Win32_Process -Filter "Name='recall.exe'" | Where-Object { $_.CommandLine -like "*$work*" }).Count) { if ($null -eq $from) { $from = $r.ms }; $to = $r.ms }
      $parts += @(Fills).Count
      Start-Sleep -Milliseconds 100
    }
    # The app traces the lane every poll: every row while the engine ran, not only the samples here.
    $during = @(Rows $s 0 2000 | Where-Object { $null -ne $from -and $_.ms -ge $from -and $_.ms -le $to -and $null -ne $_.loading.lane_ms })
    $worst = [double](($during | ForEach-Object { [double]$_.loading.lane_ms } | Measure-Object -Maximum).Maximum)
    Verdict @((Ok ($null -ne $r.stems -or $null -ne $r.loading) "$(Fmt ((Get-Item -LiteralPath $big).Length / 1GB)) GB clip streaming where it lies, $parts part files seen"), (Ok ($during.Count -ge 10 -and $worst -lt 500 -and $parts -eq 0) "a lane task waited at most $worst ms in $($during.Count) rows, over $([int]($to - $from)) ms of engine run"))
  }
}
foreach ($path in $real) {
  Run-Scenario "real drive: $(Split-Path $path -Leaf)" {
    # Where it lies: no link, so nothing on that drive is touched.
    function New-ClipLink([string]$dir, [string]$name, [string]$from) { $from }
    foreach ($tier in 'video', 'editor', 'visuals', 'tmp') { Remove-Item (Join-Path $script:cache $tier) -Recurse -Force -ErrorAction SilentlyContinue }
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
      $done = Wait-Trace $s { param($r) -not $r.loading -or ($r.stems -and $r.stems[0] -and -not @($r.loading.grey).Count) } 120000
      $checks += Ok ($null -ne $done -or (Get-Item -LiteralPath $path).Length -gt 1GB) "whole clip plus stems after $(Fmt ((Get-Date) - $t).TotalSeconds) s more"
    }
    Verdict $checks
  }
}

# A helper the app started never outlives it. Runs last, over every instance above.
Run-Scenario 'no helper outlives its app' {
  Start-Sleep -Seconds 3
  $work = [IO.Path]::GetFullPath($script:work)
  $left = @(Get-CimInstance Win32_Process -Filter "Name='ffmpeg.exe' OR Name='ffprobe.exe' OR Name='recall.exe'" | Where-Object { $_.CommandLine -like "*$work*" })
  foreach ($p in $left) { Write-Host "left: $($p.ProcessId) $($p.CommandLine)"; Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue } # ours: its arguments are in this run's folder
  Add-Result $script:scenario ($left.Count -eq 0) "$($left.Count) helpers still running after every window closed"
}

$fails = @($script:results | Where-Object { $_.Result -eq 'FAIL' }).Count
Write-Host "$($script:results.Count - $fails)/$($script:results.Count) passed"
exit $(if ($fails) { 1 } else { 0 })
