<#
  Builds optimized Recall binaries and assembles the installed app in <repo>\dist.

  dist\recall-native.exe   the app (opens the library; starts the recorder if needed)
  dist\recall-hdr.exe      recorder controller + tray (autostart points here)
  dist\recall.exe          capture engine (spawned by the controller)
  dist\recall-overlay.exe  save notice overlay
  dist\*.dll               FFmpeg runtime
  dist\bin\ffmpeg.exe, ffprobe.exe (+ DLLs)  thumbnails and scrub frames

  Usage (PowerShell):  powershell -NoProfile -File scripts\install.ps1 [-NoBuild] [-NoShortcut]
  Builds run through nice-build.ps1 (CCD1, BelowNormal) so a running game is not disturbed.
#>
param([switch]$NoBuild, [switch]$NoShortcut)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$dist = Join-Path $repo 'dist'
# Gyan FFmpeg 7.1.1 full shared build: same ABI as the 7.1 bindings, and it includes av1_nvenc.
$ffmpegRoot = Join-Path $repo 'tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared'
$nice = 'C:/Users/rohan/.claude/tools/nice-build.ps1'

if (-not $NoBuild) {
  $env:FFMPEG_DIR = $ffmpegRoot
  $env:LIBCLANG_PATH = Join-Path $repo '.tmp\nvenc-vfr\clang18\clang\native'
  $env:CARGO_BUILD_JOBS = '12'
  $env:CARGO_TARGET_DIR = Join-Path $repo 'target-dev'
  Push-Location $repo
  & powershell -NoProfile -File $nice cargo build --release -p recall -p recall-hdr -p recall-overlay --offline
  if ($LASTEXITCODE -ne 0) { Pop-Location; throw 'recorder release build failed' }
  $env:CARGO_TARGET_DIR = Join-Path $repo 'target-gpui'
  & powershell -NoProfile -File $nice cargo build --release --manifest-path (Join-Path $repo 'recall-native\Cargo.toml') --offline
  if ($LASTEXITCODE -ne 0) { Pop-Location; throw 'app release build failed' }
  Pop-Location
}

# Stop the installed copy gracefully before replacing it (controller quit closes engine, overlay and app).
$installed = Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($dist, [StringComparison]::OrdinalIgnoreCase) }
if ($installed) {
  try {
    $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', 'recall-hdr-control', [IO.Pipes.PipeDirection]::Out)
    $pipe.Connect(1500); $w = [IO.StreamWriter]::new($pipe); $w.AutoFlush = $true; $w.WriteLine('{"cmd":"quit"}'); $w.Dispose()
  } catch { }
  $deadline = (Get-Date).AddSeconds(8)
  while ((Get-Date) -lt $deadline -and (Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($dist, [StringComparison]::OrdinalIgnoreCase) })) { Start-Sleep -Milliseconds 300 }
  $left = Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($dist, [StringComparison]::OrdinalIgnoreCase) }
  if ($left) { throw ("Installed Recall is still running (" + (($left | ForEach-Object { $_.Name }) -join ', ') + "). Quit it from the tray, then re-run.") }
}

New-Item -ItemType Directory -Force -Path $dist, (Join-Path $dist 'bin') | Out-Null
$copy = @(
  @{ from = Join-Path $repo 'target-gpui\release\recall-native.exe'; to = $dist },
  @{ from = Join-Path $repo 'target-dev\release\recall-hdr.exe'; to = $dist },
  @{ from = Join-Path $repo 'target-dev\release\recall.exe'; to = $dist },
  @{ from = Join-Path $repo 'target-dev\release\recall-overlay.exe'; to = $dist }
)
foreach ($item in $copy) {
  if (-not (Test-Path $item.from)) { throw "missing build output: $($item.from)" }
  Copy-Item -LiteralPath $item.from -Destination $item.to -Force
}
Get-ChildItem (Join-Path $ffmpegRoot 'bin\*.dll') | ForEach-Object {
  Copy-Item $_.FullName $dist -Force
  Copy-Item $_.FullName (Join-Path $dist 'bin') -Force
}
Copy-Item (Join-Path $ffmpegRoot 'bin\ffmpeg.exe'), (Join-Path $ffmpegRoot 'bin\ffprobe.exe') (Join-Path $dist 'bin') -Force
Copy-Item (Join-Path $repo 'recall-native\assets\recall.ico') $dist -Force -ErrorAction SilentlyContinue

if (-not $NoShortcut) {
  $lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Recall.lnk'
  $shell = New-Object -ComObject WScript.Shell
  $s = $shell.CreateShortcut($lnk)
  $s.TargetPath = Join-Path $dist 'recall-native.exe'
  $s.WorkingDirectory = $dist
  $ico = Join-Path $dist 'recall.ico'
  if (Test-Path $ico) { $s.IconLocation = $ico }
  $s.Description = 'Recall: game clips, library and editor'
  $s.Save()
  "Start menu shortcut: $lnk"
}
Get-ChildItem $dist -File | Select-Object Name, @{ n = 'MB'; e = { [math]::Round($_.Length / 1MB, 1) } } | Format-Table -AutoSize
"Installed to $dist"
