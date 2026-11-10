<#
  Folder-watcher check (ADVERSITY F9, F2) and the RECALL_NATIVE_LAYOUT dump, on a private Recall
  instance (see viewer-check-lib.ps1): another program changes the clip folders while the library
  is open, and the tile count in the app's trace must follow. Timers start when the file is
  finished. Prints one row per case and exits 1 if any fails.

    powershell -NoProfile -File scripts\dev\watch-check.ps1 [-Clip <mp4>] [-Exe <recall-native.exe>]

  -Clip (default: <main checkout>\.tmp\viewer-check\base.mp4, see dev-paths.ps1) must be on the same volume as the repo (the burst uses hard links).
#>
param([string]$Clip = '', [string]$Exe = '', [int]$BurstFiles = 120)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
. (Join-Path $PSScriptRoot 'dev-paths.ps1')
if (-not $Clip) { $Clip = Join-Path (Get-MainCheckout $repo) '.tmp\viewer-check\base.mp4' }
. (Join-Path $PSScriptRoot 'viewer-check-lib.ps1')
Initialize-ViewerCheck $repo $Exe
Write-Host "app: $script:exe"

$today = Get-Date -Format 'yyyy-MM-dd'
$script:serial = 0
function New-Name { $script:serial++; 'WatchCheck__{0}__03-{1:00}-{2:00}' -f $today, [math]::Floor($script:serial / 60), ($script:serial % 60) }
function Clips($s) { [int](Last $s).clips }
# Milliseconds until the tile count is $want (-1: not within $limitMs).
function Wait-Clips($s, [int]$want, [int]$limitMs = 3000) {
  $t = [Diagnostics.Stopwatch]::StartNew()
  while ($t.ElapsedMilliseconds -lt $limitMs) { if ((Clips $s) -eq $want) { return [int]$t.ElapsedMilliseconds }; Start-Sleep -Milliseconds 15 }
  -1
}
function Tile($s, [string]$file) { $null -ne (Settle $s).targets."tile:$file" }

$imported = Join-Path $script:work 'watch-imported'
$away = Join-Path $script:work 'watch-away'
Remove-Item $imported, $away -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $imported 'Rocket League') | Out-Null
@{ recordings = $null; imported = @($imported) } | ConvertTo-Json | Set-Content -Encoding Ascii (Join-Path $script:sandbox 'folders.json')
Get-ChildItem (Join-Path $script:cache 'thumbs') -ErrorAction SilentlyContinue | Remove-Item -Force

$layout = Join-Path $script:work 'watch-layout.json'
Remove-Item $layout -ErrorAction SilentlyContinue
$env:RECALL_NATIVE_LAYOUT = $layout
$seed = New-Name
$s = Start-Viewer $seed $Clip -Library
Remove-Item Env:RECALL_NATIVE_LAYOUT
$lib = Split-Path $s.Clip
$sum = @()
function Row([string]$case, [bool]$pass, [string]$detail) { Add-Result $case $pass $detail }

try {
  # A writer that takes its time: no tile while the file is open.
  $name = (New-Name) + '.mp4'; $n = Clips $s
  $in = [IO.File]::OpenRead($Clip); $out = [IO.File]::Create((Join-Path $lib $name))
  $chunk = New-Object byte[] (8MB); $early = $false
  for ($i = 0; $i -lt 12; $i++) { [void]$in.Read($chunk, 0, $chunk.Length); $out.Write($chunk, 0, $chunk.Length); $out.Flush(); Start-Sleep -Milliseconds 250; if ((Clips $s) -ne $n) { $early = $true } }
  $in.Dispose(); $out.Dispose()
  $ms = Wait-Clips $s ($n + 1)
  Row 'slow writer' (-not $early -and $ms -ge 0) "no tile for 3 s while open: $(-not $early); tile $ms ms after close"

  $name = (New-Name) + '.mp4'; $n = Clips $s
  [IO.File]::Copy($Clip, (Join-Path $lib $name))
  $ms = Wait-Clips $s ($n + 1)
  Row 'plain copy' ($ms -ge 0) "tile $ms ms after the copy returned"

  $name = (New-Name) + '.mp4'; $n = Clips $s
  [IO.File]::Copy($Clip, (Join-Path $lib "$name.part"))
  Start-Sleep -Milliseconds 1500
  $held = (Clips $s) -eq $n
  Rename-Item (Join-Path $lib "$name.part") $name
  $ms = Wait-Clips $s ($n + 1)
  Start-Sleep -Milliseconds 1200
  Row 'part then rename' ($held -and $ms -ge 0 -and (Clips $s) -eq $n + 1) "no tile for .part: $held; one tile $ms ms after the rename"

  $name = (New-Name) + '.mp4'; $n = Clips $s
  [IO.File]::Copy($Clip, (Join-Path $imported "Rocket League\$name"))
  $ms = Wait-Clips $s ($n + 1)
  Row 'imported subfolder' ($ms -ge 0) "tile $ms ms after the copy returned"

  $old = (New-Name) + '.mp4'; $new = (New-Name) + '.mp4'; $n = Clips $s
  [void](New-ClipLink $lib ($old -replace '\.mp4$', '') $Clip)
  [void](Wait-Clips $s ($n + 1))
  Rename-Item (Join-Path $lib $old) $new
  Start-Sleep -Milliseconds 2000
  $renamed = (Clips $s) -eq $n + 1 -and (Tile $s $new) -and -not (Tile $s $old)
  Remove-Item (Join-Path $lib $new)
  $ms = Wait-Clips $s $n
  Row 'rename then delete' ($renamed -and $ms -ge 0) "renamed tile moved: $renamed; gone $ms ms after the delete"

  $n = Clips $s; $since = (Last $s).ms
  $thumbs = { @(Get-ChildItem (Join-Path $script:cache 'thumbs') -ErrorAction SilentlyContinue).Count }
  $before = & $thumbs
  $names = 1..$BurstFiles | ForEach-Object { New-Name }
  $t = [Diagnostics.Stopwatch]::StartNew()
  foreach ($one in $names) { New-Item -ItemType HardLink -Path (Join-Path $lib "$one.mp4") -Target $Clip | Out-Null }
  $wrote = $t.ElapsedMilliseconds
  $ms = Wait-Clips $s ($n + $BurstFiles) 8000
  Start-Sleep -Seconds 3
  $updates = @(Get-Content $s.Trace | Where-Object { $_ -match '"note":"watch: ' } | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object { $_.ms -ge $since }).Count
  $made = (& $thumbs) - $before
  Row 'burst' ($ms -ge 0 -and $updates -le 12 -and $made -lt $BurstFiles) "$BurstFiles files in $wrote ms; all listed $ms ms later; $updates list updates; $made thumbnails made (visible rows only)"

  $n = Clips $s
  Rename-Item $imported $away
  $ms = Wait-Clips $s ($n - 1) 15000
  Rename-Item $away $imported
  $back = Wait-Clips $s $n 15000
  Row 'folder away and back' ($ms -ge 0 -and $back -ge 0) "tile left $ms ms after the rename; back $back ms after the return"

  $doc = Get-Content $layout -Raw | ConvertFrom-Json
  $names = @($doc.targets.PSObject.Properties.Name)
  $tile = ($names | Where-Object { $_ -like 'tile:*' } | Select-Object -First 1)
  $box = if ($tile) { $doc.targets.$tile }
  Row 'layout dump' ($doc.route -eq 'library' -and $doc.clips -eq (Clips $s) -and $doc.scale -gt 0 -and $box.Count -eq 4 -and $box[2] -gt 0 -and $doc.window.Count -eq 2) "route $($doc.route), $($doc.clips) clips, $($names.Count) targets, first tile [$($box -join ', ')], window [$($doc.window -join ', ')]"
}
finally {
  $p = $s.Process
  if (-not $p.HasExited) { [void]$p.CloseMainWindow(); [void]$p.WaitForExit(5000) }
  $clean = $p.HasExited
  Stop-Viewer $s
}
Row 'clean exit' $clean "window close ended the process within 5 s: $clean"

Remove-Item $imported, $away, $lib -Recurse -Force -ErrorAction SilentlyContinue
$script:results | Format-Table -AutoSize -Wrap | Out-String -Width 160 | Write-Host
if ($script:results | Where-Object { $_.Result -eq 'FAIL' }) { exit 1 }
