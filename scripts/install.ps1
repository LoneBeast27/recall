<#
  Installs Recall. Builds optimized binaries into <repo>\dist (staging), then copies them to
  the install folder, which MUST be on an SSD: the app keeps its cache in <install>\cache
  (one total budget, default 2 GB), so the clip hard drive stays cold storage that is read
  silently in the background while the SSD serves everything shown on screen.

  Layout of the install folder:
    recall-native.exe   the app (opens the library; starts the recorder if needed)
    recall-hdr.exe      recorder controller + tray (Start with Windows points here)
    recall.exe          capture engine (spawned by the controller)
    recall-overlay.exe  Recall Island overlay (notices + performance stats)
    *.dll               FFmpeg runtime
    bin\ffmpeg.exe, ffprobe.exe (+ DLLs)  thumbnails and scrub frames
    cache\              created by the app; capped (Settings → Storage)
    rtss\               private copy of your installed RTSS (performance stats; see step 5b)

  Usage (PowerShell):
    powershell -NoProfile -File scripts\install.ps1 [-InstallDir <path on an SSD>] [-NoBuild] [-NoShortcut] [-NoAutostart] [-NoRtss]
  Default install folder: %LOCALAPPDATA%\Programs\Recall (per-user, no admin rights).
  After installing, the installed recorder starts and is set to start with Windows
  (Settings > General turns that off; -NoAutostart skips it).
  Builds run through nice-build.ps1 (CCD1, BelowNormal) so a running game is not disturbed.

  Performance stats (step 5b, personal install only; -NoRtss skips it): when RivaTuner
  Statistics Server is installed, its folder is copied to <install>\rtss with its own
  on-screen display off ([OSD] EnableOSD=0 in Profiles\Global and Profiles\*.cfg; RTSS's
  files are not modified), and two on-demand scheduled tasks with highest privileges are
  registered through ONE administrator prompt: "Recall RTSS start" (runs <install>\rtss\RTSS.exe)
  and "Recall RTSS stop" (stops only RTSS processes under <install>\rtss). The recorder runs
  them when stats turn on / off, without prompts. Your own "RTSS" / "MSIAfterburner" tasks and
  your RTSS in Program Files are never changed. RTSS is never copied into the repo or dist.
#>
param([string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\Recall'), [switch]$NoBuild, [switch]$NoShortcut, [switch]$NoAutostart, [switch]$NoRtss)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$stage = Join-Path $repo 'dist'
# Gyan FFmpeg 7.1.1 full shared build: same ABI as the 7.1 bindings, and it includes av1_nvenc.
$ffmpegRoot = Join-Path $repo 'tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared'
$nice = 'C:/Users/rohan/.claude/tools/nice-build.ps1'

function Get-DriveKind([string]$path) {
  $full = [IO.Path]::GetFullPath($path)
  $letter = $full.Substring(0, 1)
  $partition = Get-Partition -DriveLetter $letter -ErrorAction Stop
  $disk = Get-PhysicalDisk | Where-Object { $_.DeviceId -eq [string]$partition.DiskNumber } | Select-Object -First 1
  if (-not $disk) { return @{ kind = 'Unknown'; model = '?' } }
  $kind = if ($disk.MediaType -eq 'SSD' -or $disk.BusType -eq 'NVMe') { 'SSD' } elseif ($disk.MediaType -eq 'HDD') { 'HDD' } else { 'Unknown' }
  @{ kind = $kind; model = $disk.FriendlyName }
}

# 1. The install folder must be on an SSD (checked before any build work).
$drive = Get-DriveKind $InstallDir
if ($drive.kind -ne 'SSD') {
  throw ("Recall must be installed on an SSD so its cache loads instantly. '$InstallDir' is on " +
    "$($drive.model) ($($drive.kind)). Pick a folder on an SSD with -InstallDir.")
}
"Install folder: $InstallDir  ($($drive.model), SSD)"

# 2. Quit the running recorder gracefully, wherever it runs from (only one controller can run;
#    its quit also stops the engine and overlay). Done before building: a recorder started from
#    target-dev\release locks the files the build replaces.
function Send-Recorder([string]$line) {
  # The controller serves one pipe instance at a time: retry a connect that lands between
  # instances, and keep the client open briefly so the line is read before it disconnects.
  for ($attempt = 0; $attempt -lt 5; $attempt++) {
    try {
      $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', 'recall-hdr-control', [IO.Pipes.PipeDirection]::InOut)
      $pipe.Connect(1500); $w = [IO.StreamWriter]::new($pipe); $w.AutoFlush = $true; $w.WriteLine($line)
      Start-Sleep -Milliseconds 300; $pipe.Dispose(); return $true
    } catch { Start-Sleep -Milliseconds 400 }
  }
  $false
}
function Get-Installed { Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } }
function Get-Controller { Get-CimInstance Win32_Process -Filter "Name = 'recall-hdr.exe'" }
if (Get-Controller) { [void](Send-Recorder '{"cmd":"quit"}') }
$deadline = (Get-Date).AddSeconds(8)
while ((Get-Date) -lt $deadline -and ((Get-Installed) -or (Get-Controller))) { Start-Sleep -Milliseconds 300 }
$left = @(Get-Installed) + @(Get-Controller) | Where-Object { $_ }
if ($left) { throw ("Recall is still running (" + (($left | ForEach-Object { $_.Name }) -join ', ') + "). Quit it from the tray, then re-run.") }

# 3. Build.
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

# 4. Stage.
New-Item -ItemType Directory -Force -Path $stage, (Join-Path $stage 'bin') | Out-Null
foreach ($exe in @('target-gpui\release\recall-native.exe', 'target-dev\release\recall-hdr.exe', 'target-dev\release\recall.exe', 'target-dev\release\recall-overlay.exe')) {
  $from = Join-Path $repo $exe
  if (-not (Test-Path $from)) { throw "missing build output: $from" }
  Copy-Item -LiteralPath $from -Destination $stage -Force
}
Get-ChildItem (Join-Path $ffmpegRoot 'bin\*.dll') | ForEach-Object {
  Copy-Item $_.FullName $stage -Force
  Copy-Item $_.FullName (Join-Path $stage 'bin') -Force
}
Copy-Item (Join-Path $ffmpegRoot 'bin\ffmpeg.exe'), (Join-Path $ffmpegRoot 'bin\ffprobe.exe') (Join-Path $stage 'bin') -Force
Copy-Item (Join-Path $repo 'recall-native\assets\recall.ico') $stage -Force

# 5. Install (the cache folder is never touched).
New-Item -ItemType Directory -Force -Path $InstallDir, (Join-Path $InstallDir 'bin') | Out-Null
Get-ChildItem $stage -File | ForEach-Object { Copy-Item $_.FullName $InstallDir -Force }
Get-ChildItem (Join-Path $stage 'bin') -File | ForEach-Object { Copy-Item $_.FullName (Join-Path $InstallDir 'bin') -Force }

# 5b. RTSS for the performance-stats island (helpers in install-rtss.ps1).
if (-not $NoRtss) {
  try {
    . (Join-Path $PSScriptRoot 'install-rtss.ps1')
    Install-RecallRtss -InstallDir $InstallDir -Repo $repo
  } catch { Write-Warning "RTSS step skipped: $($_.Exception.Message). Performance stats show '-' for FPS until it is set up (re-run the installer)." }
}

if (-not $NoShortcut) {
  $lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Recall.lnk'
  $shell = New-Object -ComObject WScript.Shell
  $s = $shell.CreateShortcut($lnk)
  $s.TargetPath = Join-Path $InstallDir 'recall-native.exe'
  $s.WorkingDirectory = $InstallDir
  $s.IconLocation = Join-Path $InstallDir 'recall.ico'
  $s.Description = 'Recall: game clips, library and editor'
  $s.Save()
  "Start menu shortcut: $lnk"
}
# 6. Start the installed recorder; it owns Start with Windows (the Run value names its own path).
Start-Process -FilePath (Join-Path $InstallDir 'recall-hdr.exe') -ArgumentList '--record' -WorkingDirectory $InstallDir
$deadline = (Get-Date).AddSeconds(10); $up = $false
while (-not $up -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500; $up = Send-Recorder '{"cmd":"status"}' }
if (-not $up) { Write-Warning 'The recorder did not answer on its control pipe; start Recall from the Start menu.' }
elseif (-not $NoAutostart) {
  [void](Send-Recorder '{"cmd":"set_config","patch":{"startup":{"autostart":true}}}')
  Start-Sleep -Milliseconds 500
  $run = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'recall-hdr' -ErrorAction SilentlyContinue).'recall-hdr'
  if ($run -and $run -like "*$InstallDir*") { "Recorder running; it starts with Windows ($run)." }
  else { Write-Warning 'Start with Windows was not set; turn it on in Recall > Settings > General.' }
} else { 'Recorder running.' }
$size = (Get-ChildItem $InstallDir -Recurse -File -Exclude 'cache' | Where-Object { $_.FullName -notlike "*\cache\*" } | Measure-Object Length -Sum).Sum
"Installed to $InstallDir ($([math]::Round($size / 1MB)) MB of program files; the cache is capped separately)."
