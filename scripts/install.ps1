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
    powershell -NoProfile -File scripts\install.ps1 [-InstallDir <path on an SSD>] [-NoBuild] [-NoShortcut] [-Autostart] [-NoFrameTiming] [-CloseApp] [-AllowUnknownDrive] [-QuitWaitMinutes N]
  Default install folder: %LOCALAPPDATA%\Programs\Recall (per-user, no admin rights).
  After installing, the installed recorder starts. Start with Windows is opt-in: the installer
  leaves that setting as it is (off on a first install) unless -Autostart turns it on; Settings >
  General changes it later. An update never turns it back on. (-NoAutostart is still accepted
  and does nothing.)
  -AllowUnknownDrive: install on a drive Windows can't classify as SSD or HDD (USB SSD, RAID,
  Storage Spaces, VM disk) with a warning; a drive Windows reports as an HDD is always refused.
  A recorder finishing a clip when it is asked to quit is waited for while its status says so
  (finishing_clip), plus 2 min of grace once it does not; a re-run while it saves sends no second
  quit and joins the wait. -QuitWaitMinutes N caps that wait at N minutes for unattended runs (the
  merge gate); the run then stops with an error naming the pid and replaces nothing.
  Only this Windows session's Recall counts (another signed-in user's is left alone), and the
  recorder's control pipe is per session: \\.\pipe\recall-hdr-control-<session id>. A recorder
  from before that change (it listens on the bare recall-hdr-control) is asked to quit there too.
  Builds run through nice-build.ps1 (CCD1, BelowNormal) when it is installed, so a running game is not disturbed.
  While the installed Recall app runs, the installer refuses before doing anything (exit 3):
  replacing the program would close it under you. Close Recall and re-run, or pass -CloseApp
  to have the installer close it first.

  Performance stats (step 5b; -NoFrameTiming skips it): FPS and 1% low come from Windows frame
  timing, the present events Windows itself logs (the method Intel PresentMon and NVIDIA
  FrameView use: no hooks, nothing injected into the game). Reading them needs the local group
  Performance Log Users (SID S-1-5-32-559). When you are not in it, ONE administrator prompt
  adds you; it takes effect at your next sign-in. Declining the prompt only leaves FPS as "-".
#>
param([string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\Recall'), [switch]$NoBuild, [switch]$NoShortcut, [switch]$Autostart, [switch]$NoAutostart, [switch]$NoFrameTiming, [switch]$NoStart, [switch]$CloseApp, [switch]$AllowUnknownDrive, [int]$QuitWaitMinutes = 0, [string]$StageDir, [switch]$UpdateExisting)
$ErrorActionPreference = 'Stop'
# A relative -InstallDir or RECALL_CONTROLLER_PATH means the PowerShell location ($PWD), which
# New-Item, the swap and Start-Process use; .NET would resolve it against the process folder
# instead. Resolved once here, so every step names the same folder (audit R5-startup-4).
$InstallDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InstallDir)
$envController = if ($env:RECALL_CONTROLLER_PATH) { $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($env:RECALL_CONTROLLER_PATH) }
$repo = Split-Path $PSScriptRoot -Parent
$stage = Join-Path $repo 'dist'
# Gyan FFmpeg 7.1.1 full shared build: same ABI as the 7.1 bindings, and it includes av1_nvenc.
$buildRoot = if ($StageDir -and $env:RECALL_TOOLS_ROOT) {
  (Resolve-Path -LiteralPath $env:RECALL_TOOLS_ROOT).ProviderPath
} else { $repo }
$ffmpegRoot = Join-Path $buildRoot 'tools\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared'
# Optional low-priority build wrapper (CCD1, BelowNormal); a plain cargo build when absent.
$nice = Join-Path $env:USERPROFILE '.claude\tools\nice-build.ps1'
function Invoke-Build { if (Test-Path $nice) { & powershell -NoProfile -File $nice @args } else { & $args[0] @($args | Select-Object -Skip 1) } }

function Build-Recall {
  $env:FFMPEG_DIR = $ffmpegRoot
  if (!$env:LIBCLANG_PATH) { $env:LIBCLANG_PATH = Join-Path $buildRoot '.tmp\nvenc-vfr\clang18\clang\native' }
  $env:CARGO_BUILD_JOBS = '12'
  $env:CARGO_TARGET_DIR = Join-Path $buildRoot 'target-dev'
  Push-Location $repo
  try {
    Invoke-Build cargo build --release -p recall -p recall-hdr -p recall-overlay --offline
    if ($LASTEXITCODE -ne 0) { throw 'recorder release build failed' }
    $env:CARGO_TARGET_DIR = Join-Path $buildRoot 'target-gpui'
    Invoke-Build cargo build --release --manifest-path (Join-Path $repo 'recall-native\Cargo.toml') --offline
    if ($LASTEXITCODE -ne 0) { throw 'app release build failed' }
  } finally {
    Pop-Location
  }
}

function Copy-RecallExecutables([string]$destination) {
  foreach ($exe in 'target-gpui\release\recall-native.exe','target-dev\release\recall-hdr.exe','target-dev\release\recall.exe','target-dev\release\recall-overlay.exe') {
    $from = Join-Path $buildRoot $exe
    if (!(Test-Path -LiteralPath $from -PathType Leaf)) { throw "Missing build output: $from" }
    Copy-Item -LiteralPath $from -Destination $destination -Force
  }
}

if ($StageDir) {
  $StageDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($StageDir)
  if ($NoBuild -or $UpdateExisting -or (Test-Path -LiteralPath $StageDir)) {
    throw 'StageDir requires a build and an unused output folder.'
  }
  Build-Recall
  New-Item -ItemType Directory -Path $StageDir | Out-Null
  Copy-RecallExecutables $StageDir
  return
}

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
if ($drive.kind -eq 'HDD' -or ($drive.kind -ne 'SSD' -and -not $AllowUnknownDrive)) {
  $hint = if ($drive.kind -eq 'HDD') { '' } else { " If it is an SSD that Windows can't classify (USB, RAID, Storage Spaces, VM disk), pass -AllowUnknownDrive." }
  throw ("Recall must be installed on an SSD so its cache loads instantly. '$InstallDir' is on " +
    "$($drive.model) ($($drive.kind)). Pick a folder on an SSD with -InstallDir.$hint")
}
if ($drive.kind -eq 'SSD') { "Install folder: $InstallDir  ($($drive.model), SSD)" }
else { Write-Warning "Install folder: $InstallDir ($($drive.model)): Windows can't tell whether this is an SSD; continuing (-AllowUnknownDrive)." }

# Only this Windows session's Recall counts: another signed-in user's app and recorder are theirs.
# The recorder's control pipe is per session too (recall-hdr control_pipe.rs).
$session = (Get-Process -Id $PID).SessionId
$pipeName = "recall-hdr-control-$session"
# Paths are compared by what they name, not how they are spelled: a \\?\ prefix is stripped,
# junctions, symlinks, subst drives and 8.3 names resolve (GetFinalPathNameByHandleW when the
# path opens, else GetLongPathNameW, else the parent folder resolved plus the name), case is
# ignored by the callers. An elevated process's image path, which WMI leaves empty for a
# non-elevated caller, is read with QueryFullProcessImageNameW.
if (-not ('RecallInstall.PathId' -as [type])) {
  Add-Type -Name PathId -Namespace RecallInstall -MemberDefinition @'
[DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] public static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
[DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] public static extern uint GetFinalPathNameByHandleW(IntPtr file, System.Text.StringBuilder path, uint size, uint flags);
[DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] public static extern uint GetLongPathNameW(string shortPath, System.Text.StringBuilder longPath, uint size);
[DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] public static extern bool QueryFullProcessImageNameW(IntPtr process, uint flags, System.Text.StringBuilder name, ref uint size);
[DllImport("kernel32.dll", SetLastError = true)] public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
[DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
public static string Strip(string p) {
  if (p.StartsWith(@"\\?\UNC\")) return @"\\" + p.Substring(8);
  if (p.StartsWith(@"\\?\") || p.StartsWith(@"\??\")) return p.Substring(4);
  return p;
}
public static string Normalize(string path) {
  string full;
  try { full = System.IO.Path.GetFullPath(Strip(path)); } catch (Exception) { return Strip(path).TrimEnd('\\'); }
  // FILE_READ_ATTRIBUTES, share read/write/delete, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS (folders open too).
  IntPtr file = CreateFileW(full, 0x80, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero);
  if (file != new IntPtr(-1)) {
    try {
      var resolved = new System.Text.StringBuilder(1024);
      uint n = GetFinalPathNameByHandleW(file, resolved, 1024, 0);
      if (n > 0 && n < 1024) return Strip(resolved.ToString()).TrimEnd('\\');
    } finally { CloseHandle(file); }
  }
  var longName = new System.Text.StringBuilder(1024);
  uint m = GetLongPathNameW(full, longName, 1024);
  if (m > 0 && m < 1024) return longName.ToString().TrimEnd('\\');
  // Not there yet (a first install): resolve the folder that holds it.
  string parent = System.IO.Path.GetDirectoryName(full);
  if (parent == null) return full.TrimEnd('\\');
  return Normalize(parent).TrimEnd('\\') + "\\" + System.IO.Path.GetFileName(full);
}
public static string ImagePath(uint pid) {
  // PROCESS_QUERY_LIMITED_INFORMATION: granted on this account's elevated processes too.
  IntPtr process = OpenProcess(0x1000, false, pid);
  if (process == IntPtr.Zero) return null;
  try {
    var name = new System.Text.StringBuilder(1024); uint size = 1024;
    return QueryFullProcessImageNameW(process, 0, name, ref size) ? name.ToString() : null;
  } finally { CloseHandle(process); }
}
'@
}
function Get-ExePath($p) { if ($p.ExecutablePath) { $p.ExecutablePath } else { [RecallInstall.PathId]::ImagePath([uint32]$p.ProcessId) } }
function Format-Recorder($p) {
  $path = Get-ExePath $p
  "$($p.Name) (pid $($p.ProcessId), $(if ($path) { $path } else { 'path unreadable' }))"
}
# Which recorder this run may quit or talk to (Get-Controller below), and only through a pipe
# that controller itself serves. A sandboxed install sets RECALL_CONTROLLER_PATH (and
# RECALL_LEGACY_PIPE) so it never reaches the live recorder that owns this session's real pipe.
$controllerPath = [RecallInstall.PathId]::Normalize($(if ($envController) { $envController } else { Join-Path $InstallDir 'recall-hdr.exe' }))
$installRoot = [RecallInstall.PathId]::Normalize($InstallDir).TrimEnd('\') + '\'
# Each spelling is resolved once per run: the waits below poll every process several times a second.
$script:pathIds = @{}
function Get-PathId([string]$path) {
  if (-not $script:pathIds.ContainsKey($path)) { $script:pathIds[$path] = [RecallInstall.PathId]::Normalize($path) }
  $script:pathIds[$path]
}
function Test-InInstallDir([string]$path) { $path -and (Get-PathId $path).StartsWith($installRoot, [StringComparison]::OrdinalIgnoreCase) }
# A sandbox's controller must sit inside its own -InstallDir: the app close, the wait and the swap
# below all act on -InstallDir, so a sandbox that set only the env var would replace the real
# install and close the real app (audit R4-startup-4). Refused before anything is closed.
if ($env:RECALL_CONTROLLER_PATH -and -not (Test-InInstallDir $controllerPath)) {
  throw "RECALL_CONTROLLER_PATH ($controllerPath) is not inside -InstallDir ($InstallDir). A sandboxed install must pass its own folder as -InstallDir; nothing was closed or replaced."
}
# Nor may a sandbox name the real install folder or a folder holding it (%LOCALAPPDATA%\Programs),
# in any junction, subst or 8.3 spelling: the app close, the wait and the swap act on everything
# under -InstallDir, so it would close the real app and swap into the real folder (R5-startup-2,
# R6-startup-4). Both roots end in '\', so the prefix test is by folder, and equal counts.
$defaultRoot = [RecallInstall.PathId]::Normalize((Join-Path $env:LOCALAPPDATA 'Programs\Recall')).TrimEnd('\') + '\'
if ($env:RECALL_CONTROLLER_PATH -and $defaultRoot.StartsWith($installRoot, [StringComparison]::OrdinalIgnoreCase)) {
  throw "A sandboxed install (RECALL_CONTROLLER_PATH) can't use the real install folder or a folder that holds it ($InstallDir). Pass another -InstallDir, or unset RECALL_CONTROLLER_PATH for a real install; nothing was closed or replaced."
}

if ($UpdateExisting) {
  foreach ($name in 'recall-native.exe','recall-hdr.exe','recall.exe') {
    if (!(Test-Path -LiteralPath (Join-Path $InstallDir $name) -PathType Leaf)) {
      throw 'The selected Recall installation is incomplete or unavailable. No files were changed.'
    }
  }
}

# 2a. Never close the app under the user: refuse while the installed app runs (checked before any
#     build work). Its window counts even when minimized; a hidden instance still locks the files.
#     Run again after the build (Assert-Closed below): the build takes minutes, and an app reopened
#     meanwhile would otherwise run on from `.old` next to the new engine.
function Get-InstalledApp { Get-CimInstance Win32_Process -Filter "Name = 'recall-native.exe' AND SessionId = $session" | Where-Object { Test-InInstallDir (Get-ExePath $_) } }
function Assert-AppClosed {
  $app = @(Get-InstalledApp)
  if ($app.Count -and -not $CloseApp) {
    if (-not ('RecallInstall.Win' -as [type])) {
      Add-Type -Name Win -Namespace RecallInstall -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
public delegate bool EnumProc(IntPtr h, IntPtr l);
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
'@
    }
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
}
Assert-AppClosed

# 2. Quit the running recorder gracefully (only one controller can run; its quit also stops the
#    engine and overlay). Done before building: a recorder started from target-dev\release locks
#    the files the build replaces.
#    A real install quits every recall-hdr.exe in this session, wherever it runs from (another
#    folder, an 8.3 or junction spelling, an elevated one WMI can't read), and stops with an
#    error naming any that won't quit. One that got the quit is waited for while it says it is
#    finishing a clip, with a line each minute; one already finishing gets no second quit
#    (Request-RecorderQuit, Wait-ForQuit). An update or sandbox quits only the controller in the
#    selected installation and leaves other recorders alone (Get-Controller).
function Get-PipeServer($pipe) {
  if (-not ('RecallInstall.Pipe' -as [type])) {
    Add-Type -Name Pipe -Namespace RecallInstall -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true)] public static extern bool GetNamedPipeServerProcessId(IntPtr h, out uint pid);
'@
  }
  $id = [uint32]0
  if ([RecallInstall.Pipe]::GetNamedPipeServerProcessId($pipe.SafePipeHandle.DangerousGetHandle(), [ref]$id)) { [int]$id } else { 0 }
}
# The account a process runs as: its token user SID, else the SID the session list
# (WTSEnumerateProcessesW) gives for it, or $null when neither can be read. A same-account
# elevated recorder's token can't be opened from a non-elevated run; the list still names its
# account (as recall-native pipe_owner.rs does).
function Get-ProcessUserSid([int]$id) {
  if (-not ('RecallInstall.Account' -as [type])) {
    Add-Type -Name Account -Namespace RecallInstall -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true)] public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
[DllImport("advapi32.dll", SetLastError = true)] public static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
[DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
[StructLayout(LayoutKind.Sequential)] public struct WtsProcessInfo { public uint SessionId; public uint ProcessId; public IntPtr pProcessName; public IntPtr pUserSid; }
[DllImport("wtsapi32.dll", SetLastError = true)] public static extern bool WTSEnumerateProcessesW(IntPtr server, uint reserved, uint version, out IntPtr list, out uint count);
[DllImport("wtsapi32.dll")] public static extern void WTSFreeMemory(IntPtr memory);
public static string WtsSid(uint pid) {
  IntPtr list; uint count;
  if (!WTSEnumerateProcessesW(IntPtr.Zero, 0, 1, out list, out count)) return null;
  try {
    int size = Marshal.SizeOf(typeof(WtsProcessInfo));
    for (long i = 0; i < count; i++) {
      var info = (WtsProcessInfo)Marshal.PtrToStructure(new IntPtr(list.ToInt64() + i * size), typeof(WtsProcessInfo));
      if (info.ProcessId == pid) return info.pUserSid == IntPtr.Zero ? null : new System.Security.Principal.SecurityIdentifier(info.pUserSid).Value;
    }
    return null;
  } finally { WTSFreeMemory(list); }
}
'@
  }
  $sid = Get-TokenUserSid $id; if ($sid) { $sid } else { [RecallInstall.Account]::WtsSid([uint32]$id) }
}
function Get-TokenUserSid([int]$id) {
  # PROCESS_QUERY_LIMITED_INFORMATION, TOKEN_QUERY.
  $process = [RecallInstall.Account]::OpenProcess(0x1000, $false, [uint32]$id)
  if ($process -eq [IntPtr]::Zero) { return $null }
  try {
    $token = [IntPtr]::Zero
    if (-not [RecallInstall.Account]::OpenProcessToken($process, 8, [ref]$token)) { return $null }
    try { ([Security.Principal.WindowsIdentity]::new($token)).User.Value } finally { [void][RecallInstall.Account]::CloseHandle($token) }
  } finally { [void][RecallInstall.Account]::CloseHandle($process) }
}
$mySid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
# Each owner-check reason is warned once per run; a refusal also ends the status probe (step 6).
$script:ownerWarned = @(); $script:ownerRefused = $false
function Send-Recorder([string]$line, [string]$name = $pipeName, [int]$tries = 5) {
  # The controller serves one pipe instance at a time: retry a connect that lands between
  # instances, and keep the client open briefly so the line is read before it disconnects.
  # Nothing is written unless the pipe's server is the controller this run owns (Get-Controller):
  # another recorder on the same name (the live one, during a sandboxed install) is left alone.
  # A server running as another account, or one whose account can't be read, is refused first,
  # with one warning per reason (deep-startup-p3 a, R4-startup-2).
  for ($attempt = 0; $attempt -lt $tries; $attempt++) {
    try {
      $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', $name, [IO.Pipes.PipeDirection]::InOut)
      $pipe.Connect(1500)
      $server = Get-PipeServer $pipe
      if ($server) {
        $owner = Get-ProcessUserSid $server
        if ($owner -ne $mySid) {
          $pipe.Dispose(); $script:ownerRefused = $true
          $why = if ($owner) { "another user owns the Recall pipe ($name, pid $server runs as $owner)" } else { "can't tell which account runs the Recall pipe ($name, pid $server)" }
          if ($script:ownerWarned -notcontains $why) { $script:ownerWarned += $why; Write-Warning "${why}: nothing sent." }
          # The bare legacy name is machine-wide: another server may serve its next instance, and
          # ours the one after (R5-startup-5). This session's own pipe has one server.
          if ($name -eq $legacyPipe) { Start-Sleep -Milliseconds 300; continue }
          return $false
        }
      }
      $ours = @(Get-Controller) | Where-Object { [int]$_.ProcessId -eq $server } | Select-Object -First 1
      if (-not $server -or -not $ours) {
        $pipe.Dispose()
        if ($name -eq $legacyPipe) { Start-Sleep -Milliseconds 300; continue }
        return $false
      }
      $w = [IO.StreamWriter]::new($pipe); $w.AutoFlush = $true; $w.WriteLine($line)
      # The recorder that got this line (Stop-Recorder waits on it), with its start time.
      $script:sentTo = @{ pid = $server; created = $ours.CreationDate; pipe = $name }
      Start-Sleep -Milliseconds 300; $pipe.Dispose(); return $true
    } catch { Start-Sleep -Milliseconds 400 }
  }
  $false
}
# Every process of this session running from -InstallDir, matched by folder like Get-InstalledApp
# (alias spellings resolve; an elevated one is read with QueryFullProcessImageNameW; R5-startup-3).
function Get-Installed { Get-CimInstance Win32_Process -Filter "SessionId = $session" | Where-Object { Test-InInstallDir (Get-ExePath $_) } }
# Every controller in this session, whatever its path.
function Get-SessionController { Get-CimInstance Win32_Process -Filter "Name = 'recall-hdr.exe' AND SessionId = $session" }
# Started from $controllerPath? Compared by what the two paths name (PathId.Normalize above).
function Test-OurController($p) { $path = Get-ExePath $p; [bool]$path -and [string]::Equals([RecallInstall.PathId]::Normalize($path), $controllerPath, [StringComparison]::OrdinalIgnoreCase) }
# The controllers this run quits and waits for (step 2): every one in this session for a real
# install, only the target controller for an update or sandbox.
function Get-Controller {
  $all = @(Get-SessionController)
  if ($UpdateExisting -or $env:RECALL_CONTROLLER_PATH) { $all | Where-Object { Test-OurController $_ } } else { $all }
}
# A recorder installed before the per-session pipe listens on the bare name, so the first
# update must ask it there (audit R2-deep-startup-1). Only when a controller this run quits
# runs (Get-Controller) and serves that pipe (Send-Recorder); another account's
# old pipe gives us read access only. RECALL_LEGACY_PIPE renames it for sandboxed runs. Drop
# this fallback one release after the per-session pipe shipped.
$legacyPipe = if ($env:RECALL_LEGACY_PIPE) { $env:RECALL_LEGACY_PIPE } else { 'recall-hdr-control' }
. (Join-Path $PSScriptRoot 'install-quit-wait.ps1')
function Stop-Recorder {
  # Each phase starts clean: a refusal here must not end step 6's status probe (R5-startup-5).
  $script:ownerRefused = $false; $script:sentTo = $null
  if ($UpdateExisting -or $env:RECALL_CONTROLLER_PATH) {
    $others = @(Get-SessionController | Where-Object { -not (Test-OurController $_) })
    if ($others) { Write-Host ("Left running (not $controllerPath): " + (($others | ForEach-Object { Format-Recorder $_ }) -join ', ')) }
  }
  # One quit at most: a recorder already finishing a clip is joined, not asked again (R6-startup-1).
  $quit = Request-RecorderQuit
  Wait-ForQuit $quit $QuitWaitMinutes
  $left = @(Get-Installed) + @(Get-Controller) | Where-Object { $_ }
  if ($left) { throw ("Recall is still running (" + (($left | ForEach-Object { Format-Recorder $_ }) -join ', ') + "). Quit it from the tray, then re-run.") }
}
Stop-Recorder

# 3. Build.
if (-not $NoBuild) { Build-Recall }

# 3b. Recall may have been reopened during the build (the app also starts the recorder): check
#     again right before the swap, so nothing old keeps running from `.old`.
if (-not $NoBuild) {
  Assert-AppClosed
  Stop-Recorder
}

# 4. Stage.
New-Item -ItemType Directory -Force -Path $stage | Out-Null
# Older installs kept a second copy of every FFmpeg DLL in bin\ (about 159 MB); ffmpeg.exe now sits next to them.
$oldStageBin = Join-Path $stage 'bin'
if (Test-Path $oldStageBin) { Remove-Item -LiteralPath $oldStageBin -Recurse -Force }
Copy-RecallExecutables $stage
Get-ChildItem (Join-Path $ffmpegRoot 'bin\*.dll') | ForEach-Object {
  Copy-Item $_.FullName $stage -Force
}
Copy-Item (Join-Path $ffmpegRoot 'bin\ffmpeg.exe'), (Join-Path $ffmpegRoot 'bin\ffprobe.exe') $stage -Force
Copy-Item (Join-Path $repo 'recall-native\assets\recall.ico') $stage -Force
foreach ($notice in 'LICENSE','NOTICE.md','recall-native\assets\fonts\FONTS-LICENSE.txt','recall-native\assets\icons\LICENSE-lucide.txt','recall-native\assets\icons\brand-attribution.txt') {
  Copy-Item -LiteralPath (Join-Path $repo $notice) -Destination $stage -Force
}
$ffmpegLicense = Join-Path $repo 'FFMPEG-LICENSE.txt'
if (!(Test-Path -LiteralPath $ffmpegLicense -PathType Leaf)) { $ffmpegLicense = Join-Path $ffmpegRoot 'LICENSE' }
Copy-Item -LiteralPath $ffmpegLicense -Destination (Join-Path $stage 'FFMPEG-LICENSE.txt') -Force

# 5. Install (the cache folder is never touched).
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
# All or nothing: staged and verified beside the install, swapped by rename, rolled back on any failure
# (Recall reopened mid-install must never leave old and new programs mixed). Proof: dev\install-swap-check.ps1.
. (Join-Path $PSScriptRoot 'install-swap.ps1')
Install-Swap $stage $InstallDir
# Remove the old bin\ copy: only the FFmpeg files this installer put there, then the folder if empty.
$oldBin = Join-Path $InstallDir 'bin'
if (Test-Path $oldBin) {
  $ours = @(Get-ChildItem (Join-Path $ffmpegRoot 'bin') -File | Where-Object { $_.Extension -in '.dll', '.exe' } | ForEach-Object Name)
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
if (-not $NoFrameTiming -and -not $UpdateExisting) {
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

# A sandbox (RECALL_CONTROLLER_PATH) never repoints the user's real Start menu shortcut or Start
# with Windows entry at itself: both are skipped, with one line saying so.
if ($env:RECALL_CONTROLLER_PATH) {
  $Autostart = [switch]$false
  'Sandbox install (RECALL_CONTROLLER_PATH): skipped the Start menu shortcut and Start with Windows; the real ones are unchanged.'
} elseif (-not $NoShortcut -and -not $UpdateExisting) {
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
  # -Autostart is sent to a running recorder, and none was started (R6-startup-5).
  if ($Autostart) { Write-Warning 'Start with Windows was not set (-NoStart). Turn it on in Recall > Settings > General.' }
  $size = (Get-ChildItem $InstallDir -Recurse -File -Exclude 'cache' | Where-Object { $_.FullName -notlike "*\cache\*" } | Measure-Object Length -Sum).Sum
  "Installed to $InstallDir ($([math]::Round($size / 1MB)) MB of program files; the cache is capped separately)."
  return
}
Start-Process -FilePath (Join-Path $InstallDir 'recall-hdr.exe') -ArgumentList '--record' -WorkingDirectory $InstallDir
$deadline = (Get-Date).AddSeconds(10); $up = $false
# A refusal during Stop-Recorder (the bare legacy pipe, say) is not one here (R5-startup-5).
$script:ownerRefused = $false
while (-not $up -and -not $script:ownerRefused -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500; $up = Send-Recorder '{"cmd":"status"}' }
if (-not $up) { Write-Warning 'The recorder did not answer on its control pipe; start Recall from the Start menu.' }
# Name any other recorder still running in this session, whether or not one answered: it holds
# the one-controller lock, so the recorder just installed can't take over (R4-startup-1). A
# sandbox or an update (-UpdateExisting) leaves the live recorder alone and names it in Stop-Recorder instead.
if (-not $env:RECALL_CONTROLLER_PATH) {
  $stray = @(Get-SessionController | Where-Object { -not (Test-OurController $_) })
  # -UpdateExisting quits only its own controller and names the others in Stop-Recorder (item 12).
  if ($stray -and -not $UpdateExisting) { Write-Warning ("An older recorder is still running (" + (($stray | ForEach-Object { Format-Recorder $_ }) -join ', ') + "). Quit Recall from the tray, then start it again.") }
}
# Start with Windows is opt-in: only -Autostart sends the setting (an update never turns it on).
if ($up -and $Autostart) {
  [void](Send-Recorder '{"cmd":"set_config","patch":{"startup":{"autostart":true}}}')
  Start-Sleep -Milliseconds 500
  $run = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'recall-hdr' -ErrorAction SilentlyContinue).'recall-hdr'
  if ($run -and $run -like "*$InstallDir*") { "Recorder running; it starts with Windows ($run)." }
  else { Write-Warning 'Start with Windows was not set; turn it on in Recall > Settings > General.' }
} elseif ($up) { 'Recorder running. Start with Windows is unchanged (Recall > Settings > General; -Autostart turns it on).' }
# -Autostart is never dropped without a word.
if ($Autostart -and -not $up) { Write-Warning 'Start with Windows was not set: the recorder did not answer. Turn it on in Recall > Settings > General.' }
$size = (Get-ChildItem $InstallDir -Recurse -File -Exclude 'cache' | Where-Object { $_.FullName -notlike "*\cache\*" } | Measure-Object Length -Sum).Sum
"Installed to $InstallDir ($([math]::Round($size / 1MB)) MB of program files; the cache is capped separately)."
