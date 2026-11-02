# Real-clip check for the trimmed edited export (seek to the first range, stop after the last).
# Exports one range of a COPY of a clip with an old and a new engine, then compares what ffprobe reads back:
# first video pts, first audio pts per track, video frame count, per-stream durations.
# Run through the build gate, never while a game runs, and only on copies.
# Usage: export-trim-check.ps1 <clip copy> <start s> <end s> <old recall.exe> <new recall.exe> <ffmpeg bin dir>
$clip, $start, $end, $old, $new, $ffbin = $args
$env:PATH = "$ffbin;$env:PATH"
$work = Join-Path (Split-Path $clip) ("trim-check-" + [IO.Path]::GetFileNameWithoutExtension($clip))
New-Item -ItemType Directory -Force $work | Out-Null
$audio = @(ffprobe -v error -select_streams a -show_entries stream=index -of csv=p=0 $clip | ForEach-Object { [int]$_ })
$project = @{
  plan = @{ ranges = @(@{ start = [double]$start; end = [double]$end; speed = 1.0 }); audio = @($audio | ForEach-Object { @{ stream_index = $_; gain_db = 0.0; muted = $false } }) }
  export = @{ mode = 'reencode'; max_bytes = $null; height = $null }
}
$projectPath = Join-Path $work 'project.json'
$project | ConvertTo-Json -Depth 6 | Set-Content $projectPath

function Probe([string]$file) {
  $streams = ffprobe -v error -show_entries stream=index,codec_type,start_time,duration,nb_frames -of json $file | ConvertFrom-Json
  $firstPts = {
    param($sel)
    $line = ffprobe -v error -select_streams $sel -show_entries packet=pts_time -read_intervals '%+#1' -of csv=p=0 $file | Select-Object -First 1
    [double]"$line".Trim().TrimEnd(',')
  }
  [pscustomobject]@{
    VideoPts = & $firstPts 'v:0'
    AudioPts = @(for ($k = 0; $k -lt $audio.Count; $k++) { & $firstPts "a:$k" })
    Frames = [int](ffprobe -v error -count_packets -select_streams v:0 -show_entries stream=nb_read_packets -of csv=p=0 $file)
    Durations = @($streams.streams | ForEach-Object { [double]$_.duration })
  }
}

$results = @{}
foreach ($pair in @(@('old', $old), @('new', $new))) {
  $out = Join-Path $work "$($pair[0]).mp4"; Remove-Item $out -ErrorAction SilentlyContinue
  $watch = [Diagnostics.Stopwatch]::StartNew()
  & $pair[1] --export-edit $clip $out 60 $projectPath --json 2>$null | Out-Null
  $watch.Stop()
  if ($LASTEXITCODE -ne 0) { "FAIL $($pair[0]) engine exit $LASTEXITCODE"; exit 1 }
  $results[$pair[0]] = Probe $out
  "{0}: {1:n1}s  video pts {2}  audio pts {3}  frames {4}  durations {5}" -f $pair[0], $watch.Elapsed.TotalSeconds, $results[$pair[0]].VideoPts, ($results[$pair[0]].AudioPts -join ','), $results[$pair[0]].Frames, ($results[$pair[0]].Durations -join ',')
}
$o = $results.old; $n = $results.new
$frame = 1.0 / 60; $packet = 1024.0 / 48000
$bad = @()
if ([math]::Abs($o.VideoPts - $n.VideoPts) -gt $frame) { $bad += 'first video pts' }
for ($i = 0; $i -lt $o.AudioPts.Count; $i++) { if ([math]::Abs($o.AudioPts[$i] - $n.AudioPts[$i]) -gt $packet) { $bad += "first audio pts (track $i)" } }
if ([math]::Abs($o.Frames - $n.Frames) -gt 1) { $bad += 'frame count' }
for ($i = 0; $i -lt $o.Durations.Count; $i++) { if ([math]::Abs($o.Durations[$i] - $n.Durations[$i]) -gt [math]::Max($frame, $packet)) { $bad += "duration (stream $i)" } }
$ssim = ffmpeg -hide_banner -nostats -i (Join-Path $work 'old.mp4') -i (Join-Path $work 'new.mp4') -lavfi '[0:v][1:v]ssim' -f null - 2>&1 | Select-String 'SSIM Y'
"old vs new picture: $ssim"
if ($bad) { "MISMATCH: $($bad -join ', ')"; exit 1 } else { 'MATCH: within one frame / one audio packet' }
