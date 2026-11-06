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
    ffmpeg.exe, ffprobe.exe  thumbnails, scrub frames, light copies (use the DLLs above)
    cache\              created by the app; capped (Settings → Storage)

  Usage (PowerShell):
    powershell -NoProfile -File scripts\install.ps1 [-InstallDir <path on an SSD>] [-NoBuild] [-NoShortcut] [-NoAutostart] [-NoFrameTiming] [-CloseApp]
  Default install folder: %LOCALAPPDATA%\Programs\Recall (per-user, no admin rights).
  After installing, the installed recorder starts and is set to start with Windows
  (Settings > General turns that off; -NoAutostart skips it).
  Builds run through nice-build.ps1 (CCD1, BelowNormal) when it is installed, so a running game is not disturbed.
  While the installed Recall app runs, the installer refuses before doing anything (exit 3):
  replacing the program would close it under you. Close Recall and re-run, or pass -CloseApp
  to have the installer close it first.

  Performance stats (step 5b; -NoFrameTiming skips it): FPS and 1% low come from Windows frame
  timing, the present events Windows itself logs (the method Intel PresentMon and NVIDIA
  FrameView use: no hooks, nothing injected into the game). Reading them needs the local group
  Performance Log Users (SID S-1-5-32-559). When you are not in it, ONE administrator prompt
  adds you; it takes effect at your next sign-in. Declining the prompt only leaves FPS as "-".

  Updates and releases (see scripts\release.ps1, scripts\apply-update.ps1):
    -Stage <folder>   install a ready staged folder (an unpacked release zip); no build.
    -StageOnly        build and stage only (to -StageDir, default <repo>\dist); installs nothing.
#>
param([string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\Recall'), [switch]$NoBuild, [switch]$NoShortcut, [switch]$NoAutostart, [switch]$NoFrameTiming, [switch]$NoStart, [switch]$CloseApp,
  [string]$Stage, [switch]$StageOnly, [string]$StageDir)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$stage = if ($Stage) { $Stage } elseif ($StageDir) { $StageDir } else { Join-Path $repo 'dist' }
# Gyan FFmpeg 7.1.1 full shared build: same ABI as the 7.1 bindings, and it includes av1_nvenc.
$ffmpegRoot = Join-Path $repo 'tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared'
# Optional low-priority build wrapper (CCD1, BelowNormal); a plain cargo build when absent.
$nice = Join-Path $env:USERPROFILE '.claude\tools\nice-build.ps1'
function Invoke-Build { if (Test-Path $nice) { & powershell -NoProfile -File $nice @args } else { & $args[0] @($args | Select-Object -Skip 1) } }

function Get-DriveKind([string]$path) {
  $full = [IO.Path]::GetFullPath($path)
  $letter = $full.Substring(0, 1)
  $partition = Get-Partition -DriveLetter $letter -ErrorAction Stop
  $disk = Get-PhysicalDisk | Where-Object { $_.DeviceId -eq [string]$partition.DiskNumber } | Select-Object -First 1
  if (-not $disk) { return @{ kind = 'Unknown'; model = '?' } }
  $kind = if ($disk.MediaType -eq 'SSD' -or $disk.BusType -eq 'NVMe') { 'SSD' } elseif ($disk.MediaType -eq 'HDD') { 'HDD' } else { 'Unknown' }
  @{ kind = $kind; model = $disk.FriendlyName }
}

function Invoke-RecallBuild {
  $env:FFMPEG_DIR = $ffmpegRoot
  $env:LIBCLANG_PATH = Join-Path $repo '.tmp\nvenc-vfr\clang18\clang\native'
  $env:CARGO_BUILD_JOBS = '12'
  $env:CARGO_TARGET_DIR = Join-Path $repo 'target-dev'
  Push-Location $repo
  Invoke-Build cargo build --release -p recall -p recall-hdr -p recall-overlay --offline
  if ($LASTEXITCODE -ne 0) { Pop-Location; throw 'recorder release build failed' }
  $env:CARGO_TARGET_DIR = Join-Path $repo 'target-gpui'
  Invoke-Build cargo build --release --manifest-path (Join-Path $repo 'recall-native\Cargo.toml') --offline
  if ($LASTEXITCODE -ne 0) { Pop-Location; throw 'app release build failed' }
  Pop-Location
}

function Invoke-RecallStage {
  New-Item -ItemType Directory -Force -Path $stage | Out-Null
  # Older installs kept a second copy of every FFmpeg DLL in bin\ (about 159 MB); ffmpeg.exe now sits next to them.
  $oldStageBin = Join-Path $stage 'bin'
  if (Test-Path $oldStageBin) { Remove-Item -LiteralPath $oldStageBin -Recurse -Force }
  foreach ($exe in @('target-gpui\release\recall-native.exe', 'target-dev\release\recall-hdr.exe', 'target-dev\release\recall.exe', 'target-dev\release\recall-overlay.exe')) {
    $from = Join-Path $repo $exe
    if (-not (Test-Path $from)) { throw "missing build output: $from" }
    Copy-Item -LiteralPath $from -Destination $stage -Force
  }
  Get-ChildItem (Join-Path $ffmpegRoot 'bin\*.dll') | ForEach-Object {
    Copy-Item $_.FullName $stage -Force
  }
  Copy-Item (Join-Path $ffmpegRoot 'bin\ffmpeg.exe'), (Join-Path $ffmpegRoot 'bin\ffprobe.exe') $stage -Force
  Copy-Item (Join-Path $repo 'recall-native\assets\recall.ico') $stage -Force
  # The in-app updater runs these from the install folder (services/update.rs).
  foreach ($s in 'install.ps1', 'install-swap.ps1', 'apply-update.ps1') { Copy-Item (Join-Path $PSScriptRoot $s) $stage -Force }
}

# 0. Release builds: build and stage only, touch no install.
if ($StageOnly) {
  Invoke-RecallBuild
  Invoke-RecallStage
  "Staged to $stage"
  return
}

# 1. The install folder must be on an SSD (checked before any build work).
$drive = Get-DriveKind $InstallDir
if ($drive.kind -ne 'SSD') {
  throw ("Recall must be installed on an SSD so its cache loads instantly. '$InstallDir' is on " +
    "$($drive.model) ($($drive.kind)). Pick a folder on an SSD with -InstallDir.")
}
"Install folder: $InstallDir  ($($drive.model), SSD)"

# 2a. Never close the app under the user: refuse while the installed app runs (checked before any
#     build work). Its window counts even when minimized; a hidden instance still locks the files.
function Get-InstalledApp { Get-CimInstance Win32_Process -Filter "Name = 'recall-native.exe'" | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } }
$app = @(Get-InstalledApp)
if ($app.Count -and -not $CloseApp) {
  Add-Type -Name Win -Namespace RecallInstall -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
public delegate bool EnumProc(IntPtr h, IntPtr l);
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
'@
  $ids = @($app | ForEach-Object { [int]$_.ProcessId }); $script:shown = 0
  [void][RecallInstall.Win]::EnumWindows({ param($h, $l) $p = [uint32]0; [void][RecallInstall.Win]::GetWindowThreadProcessId($h, [ref]$p); if ($ids -contains [int]$p -and [RecallInstall.Win]::IsWindowVisible($h)) { $script:shown++ }; $true }, [IntPtr]::Zero)
  $how = if ($script:shown) { "is open ($($script:shown) window(s))" } else { 'is running in the background' }
  Write-Host "Recall $how (pid $($ids -join ', ')). Installing now would close it. Close Recall, then re-run; or pass -CloseApp to close it first."
  exit 3
}
if ($app.Count) {
  # -CloseApp: ask each instance to close the way its own X button does, then wait below.
  foreach ($p in $app) { try { [void](Get-Process -Id $p.ProcessId -ErrorAction Stop).CloseMainWindow() } catch { } }
}

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

# 3. Build, 4. Stage (skipped for -Stage: that folder is already staged, e.g. an unpacked update).
if (-not $Stage) {
  if (-not $NoBuild) { Invoke-RecallBuild }
  Invoke-RecallStage
} elseif (-not (Test-Path (Join-Path $stage 'recall-native.exe'))) {
  throw "not a staged Recall folder: $stage"
}

# 5. Install (the cache folder is never touched).
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
# All or nothing: staged and verified beside the install, swapped by rename, rolled back on any failure
# (Recall reopened mid-install must never leave old and new programs mixed). Proof: dev\install-swap-check.ps1.
. (Join-Path $PSScriptRoot 'install-swap.ps1')
Install-Swap $stage $InstallDir
# Remove the old bin\ copy: only the FFmpeg files this installer put there, then the folder if empty.
$oldBin = Join-Path $InstallDir 'bin'
if (Test-Path $oldBin) {
  $ffBin = if ($Stage) { $stage } else { Join-Path $ffmpegRoot 'bin' }
  $ours = @(Get-ChildItem $ffBin -File | Where-Object { $_.Extension -in '.dll', '.exe' } | ForEach-Object Name)
  Get-ChildItem $oldBin -File | Where-Object { $ours -contains $_.Name } | ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force }
  if (-not (Get-ChildItem $oldBin -Force)) { Remove-Item -LiteralPath $oldBin -Force }
}

# 5b. Frame timing for the performance-stats island: membership in Performance Log Users.
$perfLogUsers = 'S-1-5-32-559'
function Test-PerfLogMember([string]$userSid) {
  # Already in the group (whether or not this sign-in's token has it yet).
  try { [bool](Get-LocalGroupMember -SID $perfLogUsers -ErrorAction Stop | Where-Object { $_.SID.Value -eq $userSid }) }
  catch { $false }
}
if (-not $NoFrameTiming) {
  $me = [Security.Principal.WindowsIdentity]::GetCurrent()
  $userSid = $me.User.Value
  if ($me.Groups | Where-Object { $_.Value -eq $perfLogUsers }) {
    'Frame timing: you are in Performance Log Users; the stats island shows FPS.'
  } elseif (Test-PerfLogMember $userSid) {
    'Frame timing: you are in Performance Log Users; FPS shows after your next sign-in.'
  } else {
    # One elevated prompt runs exactly this; an existing membership counts as success.
    $add = "try { Add-LocalGroupMember -SID '$perfLogUsers' -Member '$userSid' -ErrorAction Stop; exit 0 } " +
      "catch { if (`$_.FullyQualifiedErrorId -like 'MemberExists*') { exit 0 }; exit 1 }"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($add))
    try {
      $elevated = Start-Process powershell -Verb RunAs -Wait -PassThru -WindowStyle Hidden `
        -ArgumentList @('-NoProfile', '-NonInteractive', '-EncodedCommand', $encoded)
      if ($elevated.ExitCode -eq 0 -and (Test-PerfLogMember $userSid)) {
        'Frame timing: added you to Performance Log Users. FPS shows after your next sign-in.'
      } else {
        Write-Warning "Frame timing: adding you to Performance Log Users failed (exit $($elevated.ExitCode)). FPS shows '-' until it is done (re-run the installer)."
      }
    } catch {
      Write-Warning "Frame timing: the administrator prompt was declined ($($_.Exception.Message)). FPS shows '-' until you re-run the installer."
    }
  }
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
#    -NoStart leaves it stopped (e.g. mid-match: starting a capture can hitch the game once).
if ($NoStart) {
  'Recorder left stopped (-NoStart); start Recall from the Start menu.'
  $size = (Get-ChildItem $InstallDir -Recurse -File -Exclude 'cache' | Where-Object { $_.FullName -notlike "*\cache\*" } | Measure-Object Length -Sum).Sum
  "Installed to $InstallDir ($([math]::Round($size / 1MB)) MB of program files; the cache is capped separately)."
  return
}
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
