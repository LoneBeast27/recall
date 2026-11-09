<#
  Viewer response checks for the editor's player (dev check; run after player/editor changes).

  Each scenario opens a fresh hard link of a test clip in a private Recall instance (edits never
  touch the original; the instance runs outside every monitor, never takes focus, and uses its own
  settings, library, cache and control pipe), drives it the way a person does -- keys, the play
  button, the timeline, the tools, the Library -- and reads the app's trace (RECALL_NATIVE_TRACE)
  to check that the clock AND the picture move, start where they should, and stop where they
  should. Prints one row per scenario and exits 1 if any fails.

    powershell -NoProfile -File scripts\dev\viewer-check.ps1 [-Clip <mp4>] [-ShortClip <mp4>]
      [-MomentsClip <mp4>] [-Exe <recall-native.exe>] [-Only '<name pattern>', ...]

  Clips (copied once into .tmp\viewer-check, which must be on the same volume as the repo):
    base     a clip of at least 60 s; default the newest VALORANT clip over 100 MB in paths.clipLibrary
    short    ~10 s with two sound tracks; default the first 10.45 s of base. The 2026-10-30 reports
             used Recall-Test-3 (sound 10.453 s, picture 10.438 s): pass it with -ShortClip.
    moments  a SteelSeries Moments clip (audio stamps 1000/1100 ticks apart); default the smallest
             2025 VALORANT clip over 60 MB in paths.clipLibrary (.local/machine.json,
             written by init-machine.ps1 -ClipLibrary)
    derived from short with ffmpeg (once): no sound, 0.8 s long, sound 0.5 s shorter than the
    picture, and an AV1 copy (the player needs a preview copy when Windows cannot decode AV1).
  Needs the engine (recall.exe; RECALL_ENGINE or a built target-dev) and ffmpeg/ffprobe on PATH.
#>
param([string]$Clip = '', [string]$ShortClip = '', [string]$MomentsClip = '', [string]$Exe = '', [string[]]$Only = @())
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
. (Join-Path $PSScriptRoot 'viewer-check-lib.ps1')
Initialize-ViewerCheck $repo $Exe
. (Join-Path $PSScriptRoot 'dev-paths.ps1')
$script:only = @($Only | ForEach-Object { if ($_ -like '*|*') { $_ -split '\|' } else { $_ -split ',' } } | Where-Object { $_ })  # '|' between names lets a name hold a comma # -File passes 'a,b' as one string
Write-Host "player: $script:exe"

$base = Join-Path $script:work 'base.mp4'
if ($Clip) { Copy-Item $Clip $base -Force }
elseif (-not (Test-Path $base)) {
  $src = Get-ChildItem (Get-MachineValue $repo 'paths.clipLibrary') -Filter 'VALORANT__*.mp4' | Where-Object { $_.Length -gt 100MB } | Sort-Object LastWriteTime -Descending | Select-Object -First 1
  Copy-Item $src.FullName $base
}
$moments = Join-Path $script:work 'moments.mp4'
if ($MomentsClip) { Copy-Item $MomentsClip $moments -Force }
elseif (-not (Test-Path $moments)) {
  $src = Get-ChildItem (Get-MachineValue $repo 'paths.clipLibrary') -Filter 'VALORANT__2025-*.mp4' | Where-Object { $_.Length -gt 60MB } | Sort-Object Length | Select-Object -First 1
  Copy-Item $src.FullName $moments
}
$short = Join-Path $script:work 'short.mp4'
if ($ShortClip) { Copy-Item $ShortClip $short -Force }
elseif (-not (Test-Path $short)) { Invoke-Ffmpeg @('-i', $base, '-map', '0', '-t', '10.45', '-c', 'copy', $short) }
$derived = [ordered]@{
  'noaudio.mp4'    = @('-map', '0:v', '-c', 'copy')
  'tiny.mp4'       = @('-map', '0', '-t', '0.8', '-c', 'copy')
  'shortsound.mp4' = @('-map', '0:v', '-map', '0:a', '-c:v', 'copy', '-filter:a', 'atrim=end=9.9', '-c:a', 'aac')
  'av1.mp4'        = @('-map', '0:v', '-map', '0:a', '-vf', 'scale=1280:-2', '-c:v', 'libsvtav1', '-preset', '10', '-c:a', 'copy')
}
foreach ($name in $derived.Keys) {
  $out = Join-Path $script:work $name
  if (-not (Test-Path $out)) { Invoke-Ffmpeg (@('-i', $short) + $derived[$name] + @($out)) }
}
$clips = @{ base = $base; short = $short; moments = $moments }
foreach ($name in $derived.Keys) { $clips[[IO.Path]::GetFileNameWithoutExtension($name)] = Join-Path $script:work $name }

. (Join-Path $PSScriptRoot 'viewer-check-play.ps1')
. (Join-Path $PSScriptRoot 'viewer-check-edit.ps1')
. (Join-Path $PSScriptRoot 'viewer-check-card.ps1')

$script:results | Format-Table -AutoSize -Wrap | Out-String -Width 220
$failed = @($script:results | Where-Object { $_.Result -eq 'FAIL' }).Count
Write-Host ("{0} scenarios, {1} failed" -f $script:results.Count, $failed)
if ($failed) { exit 1 }
