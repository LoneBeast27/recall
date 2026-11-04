<#
  Light copy -> full resolution hand-over, end to end (dev check; run after changes to the light
  copy, the streamed open, the hand-over or the loading states).

  Each scenario opens a fresh hard link of a test clip in a private, offscreen, silent Recall with
  RECALL_SLOW_DRIVE=<MB/s> (the clip's drive acts like a hard drive), an empty cache, and a light
  copy of the clip placed in the cache the way the warm-up leaves it (`light/<id>.mp4`, HEVC NVENC
  at the product's settings). It reads the app's trace and player events:

    - streamed open: the light copy is on screen at once; once the fill is done and the stems are
      in, full resolution takes over exactly once (one "takes over" event, then none over many
      ticks), the playback path is the full copy, loading ends, the pre-encode trigger becomes eligible.
    - cached open: a clip whose whole copy is already on the SSD opens on it; no light copy, no
      hand-over, loading never starts, the pre-encode trigger becomes eligible.
    - drag: while a timeline drag is held the hand-over waits, even with everything in; on release
      it happens once.
    - pre-encode: after the takeover an untouched clip is not prepared; a trim prepares it once into
      cache\tmp\pre-encode, the engine at below-normal priority on the upper CPUs (0xFFFF0000); while
      the recorder is busy (RECALL_FAKE_RECORDING) or off (RECALL_FAKE_NO_RECORDER) it waits, and
      starts once it is back.

    powershell -NoProfile -File scripts\dev\light-handover-check.ps1 [-Clip <mp4>] [-Exe <recall-native.exe>] [-Engine <recall.exe>] [-Only '<pattern>', ...]

  -Clip: at least 60 s and 100 MB; default .tmp\viewer-check\base.mp4 (copy one there first).
  -Engine: an engine stamped recall-engine-caps with FLAC stems (media cache version 6).
  Needs ffmpeg.exe with hevc_nvenc on PATH, and an HEVC decoder for Media Foundation.
#>
param([string]$Clip = '', [string]$Exe = '', [string]$Engine = 'L:\General\recall\target-dev\release\recall.exe', [string[]]$Only = @())
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
. (Join-Path $PSScriptRoot 'viewer-check-lib.ps1')
Initialize-ViewerCheck $repo $Exe
$script:only = @($Only | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
Write-Host "player: $script:exe"
if (-not $Clip) { $Clip = Join-Path $script:work 'base.mp4' }
if (-not (Test-Path $Clip)) { throw "test clip $Clip is missing" }
if (-not (Select-String -Path $Engine -Pattern 'recall-engine-caps:' -SimpleMatch -Quiet)) { throw "$Engine has no caps stamp: the app refuses it" }
$env:RECALL_ENGINE = $Engine
$size = (Get-Item $Clip).Length
$mtime = ([DateTimeOffset](Get-Item $Clip).LastWriteTimeUtc).ToUnixTimeSeconds()

# The cache key of a clip revision (`storage::identity`: FNV-1a over the lowercase name, a zero,
# the size and the mtime seconds, little endian).
Add-Type -TypeDefinition @'
public static class ClipIdentity {
  public static string Of(string name, ulong bytes, ulong secs) {
    ulong hash = 0xcbf29ce484222325UL;
    var data = new System.Collections.Generic.List<byte>(System.Text.Encoding.UTF8.GetBytes(name.ToLowerInvariant()));
    data.Add(0);
    data.AddRange(System.BitConverter.GetBytes(bytes));
    data.AddRange(System.BitConverter.GetBytes(secs));
    foreach (var b in data) { hash ^= b; unchecked { hash *= 0x100000001b3UL; } }
    return hash.ToString("x16");
  }
}
'@
function Id([string]$name) { [ClipIdentity]::Of("$name.mp4", [uint64]$size, [uint64]$mtime) }

# One light copy of the clip, made once with the product's encoder settings (`light::arguments`).
$lightSource = Join-Path $script:work 'light-of-base.mp4'
if (-not (Test-Path $lightSource) -or (Get-Item $lightSource).LastWriteTime -lt (Get-Item $Clip).LastWriteTime) {
  Write-Host 'making the light copy (HEVC NVENC, 10 Mbit/s)...'
  Invoke-Ffmpeg @('-hwaccel', 'd3d11va', '-threads', '4', '-i', $Clip, '-map', '0:v:0', '-an', '-sn', '-dn', '-fps_mode', 'passthrough', '-c:v', 'hevc_nvenc', '-preset', 'p4', '-tune', 'hq', '-rc', 'vbr', '-multipass', 'qres', '-b:v', '10M', '-maxrate', '12500k', '-bufsize', '20M', '-tag:v', 'hvc1', '-movflags', '+faststart', '-f', 'mp4', $lightSource)
}

function Clear-Cache {
  foreach ($tier in 'video', 'editor', 'visuals', 'tmp', 'light', 'heads') { Remove-Item (Join-Path $script:cache $tier) -Recurse -Force -ErrorAction SilentlyContinue }
}
function Place([string]$tier, [string]$name, [string]$from) {
  $dir = Join-Path $script:cache $tier
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $to = Join-Path $dir "$(Id $name).mp4"
  Copy-Item $from $to -Force
  $to
}
function Start-Slow([string]$name, [int]$rate) {
  $env:RECALL_SLOW_DRIVE = "$rate"
  try { $s = Start-Viewer $name $Clip } finally { Remove-Item Env:RECALL_SLOW_DRIVE -ErrorAction SilentlyContinue }
  $script:current = $s
  $s
}
# "Full resolution takes over" player events in the scenario's trace.
function Takeovers($s) { @(Select-String -Path $s.Trace -SimpleMatch 'full resolution takes over' -ErrorAction SilentlyContinue).Count }
# A few seconds of app ticks (the poll runs every ~30 ms): returns the last line.
function Ticks($s, [int]$seconds) { Start-Sleep -Seconds $seconds; Settle $s }

Run-Scenario 'a streamed open shows the light copy, then full resolution takes over once' {
  Clear-Cache
  $name = 'handover-stream'
  $light = Place 'light' $name $lightSource
  $s = Start-Slow $name 60
  $first = Last $s
  $shown = Ok ($first.preview_quality -eq $true) "light copy on screen at open: $($first.preview_quality), loading $($null -ne $first.loading)"
  $ended = Wait-Trace $s { param($r) -not $r.loading } 180000
  $loaded = Ok ($null -ne $ended) "loading ended: $($null -ne $ended)"
  $once = Takeovers $s
  $r = Ticks $s 4
  $still = Takeovers $s
  $one = Ok ($once -eq 1 -and $still -eq 1) "takeover events: $once, after 4 s of ticks: $still"
  $full = Ok ($r.source -like '*\video\*.mp4' -and $r.source -notlike '*\light\*') "playback path: $($r.source)"
  $gone = Ok ($r.preview_quality -eq $false -and $null -eq $r.loading) "preview quality $($r.preview_quality), loading $($null -ne $r.loading)"
  $armed = Ok ($r.pre_encode_eligible -eq $true) "pre-encode trigger eligible: $($r.pre_encode_eligible)"
  $plays = Press-Play $s
  Verdict @($shown, $loaded, $one, $full, $gone, $armed, $plays)
}

Run-Scenario 'a clip already on the SSD opens on its whole copy, no hand-over' {
  Clear-Cache
  $name = 'handover-cached'
  [void](Place 'light' $name $lightSource)
  $whole = Place 'video' $name $Clip
  $s = Start-Slow $name 60
  $first = Last $s
  $r = Wait-Trace $s { param($r) $r.pre_encode_eligible } 120000
  $armed = Ok ($null -ne $r) "pre-encode trigger eligible: $($null -ne $r)"
  $r = Ticks $s 3
  $none = Ok ((Takeovers $s) -eq 0) "takeover events: $(Takeovers $s)"
  $path = Ok ($r.source -eq $whole) "playback path: $($r.source)"
  $plain = Ok ($first.preview_quality -eq $false -and $r.preview_quality -eq $false -and $null -eq $r.loading) "preview quality at open $($first.preview_quality), now $($r.preview_quality); loading $($null -ne $r.loading)"
  Verdict @($armed, $none, $path, $plain)
}

Run-Scenario 'a held timeline drag holds the hand-over until it ends' {
  Clear-Cache
  $name = 'handover-drag'
  [void](Place 'light' $name $lightSource)
  $s = Start-Slow $name 40
  $tracks = Target $s 'tracks'; $y = $tracks.Top + 30; $x = TrackX $s 5
  Mouse $s 0x200 $x $y 0; Start-Sleep -Milliseconds 30
  Mouse $s 0x201 $x $y 1; Start-Sleep -Milliseconds 30
  for ($n = 1; $n -le 6; $n++) { Mouse $s 0x200 ($x + $n * 4) $y 1; Start-Sleep -Milliseconds 16 }
  $held = Wait-Trace $s { param($r) $r.drag } 3000
  $dragging = Ok ($null -ne $held) "drag held: $($null -ne $held)"
  # Everything in: the fill is done and the stems arrived (their paths are set), drag still held.
  $ready = Wait-Trace $s { param($r) $r.drag -and $r.loading -and $r.loading.share -ge 1 -and @($r.stems | Where-Object { $_ }).Count -gt 0 } 180000
  $r = Ticks $s 2
  $waited = Ok ($null -ne $ready -and $r.drag -and (Takeovers $s) -eq 0 -and $r.preview_quality -eq $true) "everything in while dragging: $($null -ne $ready); takeovers $(Takeovers $s), preview quality $($r.preview_quality)"
  Mouse $s 0x202 ($x + 24) $y 0
  $after = Wait-Trace $s { param($r) -not $r.drag -and -not $r.loading } 30000
  $r = Ticks $s 3
  $swapped = Ok ($null -ne $after -and (Takeovers $s) -eq 1 -and $r.source -like '*\video\*.mp4') "after release: loading ended $($null -ne $after), takeovers $(Takeovers $s), path $($r.source)"
  Verdict @($dragging, $waited, $swapped)
}

# The pre-encode's engine, if one runs for this check's cache: (process, priority, affinity).
function Pre-EncodeEngine {
  $cache = [IO.Path]::GetFullPath($script:cache)
  $p = Get-CimInstance Win32_Process -Filter "Name='recall.exe'" | Where-Object { $_.CommandLine -like '*export-edit*' -and $_.CommandLine -like "*$cache*" } | Select-Object -First 1
  if (-not $p) { return $null }
  try { $proc = Get-Process -Id $p.ProcessId -ErrorAction Stop; [pscustomobject]@{ Id = $p.ProcessId; Priority = "$($proc.PriorityClass)"; Affinity = ('0x{0:X}' -f [int64]$proc.ProcessorAffinity) } } catch { $null }
}
# Trace rows where the pre-encode line went from not running to running.
function Pre-EncodeStarts($s) {
  $rows = @(Get-Content $s.Trace | ForEach-Object { try { $_ | ConvertFrom-Json } catch { } } | Where-Object { $_ -and $_.route })
  $starts = 0; $was = $false
  foreach ($r in $rows) { $now = "$($r.pre_encode)" -like 'Preparing export*'; if ($now -and -not $was) { $starts++ }; $was = $now }
  $starts
}
function Pre-EncodeFiles { @(Get-ChildItem (Join-Path $script:cache 'tmp\pre-encode') -Filter '*.mp4' -ErrorAction SilentlyContinue) }

Run-Scenario 'after the takeover an edit pre-encodes once, pinned and below normal' {
  Clear-Cache
  $name = 'handover-preencode'
  [void](Place 'light' $name $lightSource)
  $s = Start-Slow $name 60
  $loaded = Ok ($null -ne (Wait-Trace $s { param($r) -not $r.loading -and $r.pre_encode_eligible } 180000)) 'loaded and eligible'
  $untouched = Ticks $s 3
  $idle = Ok ($null -eq $untouched.pre_encode -and (Pre-EncodeStarts $s) -eq 0) "an untouched clip: no pre-encode ($($untouched.pre_encode))"
  DragHandle $s 'trim-start' 20
  $engine = $null
  $t = Get-Date
  while (((Get-Date) - $t).TotalSeconds -lt 240) {
    if (-not $engine) { $engine = Pre-EncodeEngine }
    $r = Last $s
    if ("$($r.pre_encode)" -like 'Ready*') { break }
    if ("$($r.pre_encode)" -like "Couldn't*") { break }
    Start-Sleep -Milliseconds 200
  }
  $r = Ticks $s 4
  $ready = Ok ("$($r.pre_encode)" -like 'Ready*') "pre-encode line: '$($r.pre_encode)' after $([int]((Get-Date) - $t).TotalSeconds) s"
  $files = Pre-EncodeFiles
  $file = Ok ($files.Count -eq 1 -and $files[0].Length -gt 1MB) "files in cache\tmp\pre-encode: $($files.Count) ($(if ($files) { [int]($files[0].Length / 1MB) } else { 0 }) MB)"
  $starts = Pre-EncodeStarts $s
  $once = Ok ($starts -eq 1) "pre-encode starts: $starts, still Ready 4 s later: $("$($r.pre_encode)" -like 'Ready*')"
  $pinned = Ok ($engine -and $engine.Priority -eq 'BelowNormal' -and $engine.Affinity -eq '0xFFFF0000') "engine: $(if ($engine) { "priority $($engine.Priority), affinity $($engine.Affinity)" } else { 'never seen' })"
  Verdict @($loaded, $idle, $ready, $file, $once, $pinned)
}

Run-Scenario 'no pre-encode while the recorder is busy; it starts once recording stops' {
  Clear-Cache
  $name = 'handover-busy'
  $busy = Join-Path $script:work 'fake-recording'
  Set-Content $busy 'recording' -Encoding ascii
  $env:RECALL_FAKE_RECORDING = $busy
  try { $s = Start-Slow $name 120 } finally { Remove-Item Env:RECALL_FAKE_RECORDING -ErrorAction SilentlyContinue }
  try {
    $loaded = Ok ($null -ne (Wait-Trace $s { param($r) -not $r.loading -and $r.pre_encode_eligible } 180000)) 'loaded while "recording"'
    DragHandle $s 'trim-start' 20
    $r = Ticks $s 5
    $waits = Ok ("$($r.pre_encode)" -like '*once recording stops*' -and (Pre-EncodeStarts $s) -eq 0 -and -not (Pre-EncodeEngine)) "while busy: '$($r.pre_encode)', starts $(Pre-EncodeStarts $s)"
  } finally { Remove-Item $busy -ErrorAction SilentlyContinue }
  $started = Wait-Trace $s { param($r) "$($r.pre_encode)" -like 'Preparing export*' -or "$($r.pre_encode)" -like 'Ready*' } 15000
  $resumes = Ok ($null -ne $started) "after recording stops: '$($started.pre_encode)'"
  $done = Wait-Trace $s { param($r) "$($r.pre_encode)" -like 'Ready*' } 240000
  $ready = Ok ($null -ne $done -and (Pre-EncodeStarts $s) -eq 1) "ready: $($null -ne $done), starts $(Pre-EncodeStarts $s)"
  Verdict @($loaded, $waits, $resumes, $ready)
}

Run-Scenario 'no pre-encode while the recorder is off; it starts once the recorder is back' {
  Clear-Cache
  $name = 'handover-norecorder'
  $off = Join-Path $script:work 'fake-no-recorder'
  Set-Content $off 'off' -Encoding ascii
  $env:RECALL_FAKE_NO_RECORDER = $off
  try { $s = Start-Slow $name 120 } finally { Remove-Item Env:RECALL_FAKE_NO_RECORDER -ErrorAction SilentlyContinue }
  try {
    $loaded = Ok ($null -ne (Wait-Trace $s { param($r) -not $r.loading -and $r.pre_encode_eligible } 180000)) 'loaded with no recorder'
    DragHandle $s 'trim-start' 20
    $r = Ticks $s 5
    $waits = Ok ("$($r.pre_encode)" -like '*once the recorder is running*' -and (Pre-EncodeStarts $s) -eq 0 -and -not (Pre-EncodeEngine)) "without the recorder: '$($r.pre_encode)', starts $(Pre-EncodeStarts $s)"
  } finally { Remove-Item $off -ErrorAction SilentlyContinue }
  $started = Wait-Trace $s { param($r) "$($r.pre_encode)" -like 'Preparing export*' -or "$($r.pre_encode)" -like 'Ready*' } 15000
  $resumes = Ok ($null -ne $started) "recorder back: '$($started.pre_encode)'"
  $done = Wait-Trace $s { param($r) "$($r.pre_encode)" -like 'Ready*' } 240000
  $ready = Ok ($null -ne $done -and (Pre-EncodeStarts $s) -eq 1) "ready: $($null -ne $done), starts $(Pre-EncodeStarts $s)"
  Verdict @($loaded, $waits, $resumes, $ready)
}

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
