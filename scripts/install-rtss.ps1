<#
  RTSS step of scripts\install.ps1 (dot-sourced; defines functions only, runs nothing).

  Recall's RTSS lives in %ProgramFiles%\Recall\rtss: admin-only writable (default inherited
  ACLs), so the highest-privilege task that starts it cannot be hijacked by swapping the exe
  (a user-writable copy would hand any same-user program admin rights with no prompt). It is
  NEVER written into %LOCALAPPDATA%\Programs\Recall, the repo or dist.

  Install-RecallRtss runs ONE administrator prompt only when needed (copy missing, RTSS updated,
  profile edits revised, or a task missing). The elevated step (Invoke-RecallRtssElevated):
    1. stops Recall's own RTSS (only processes under %ProgramFiles%\Recall\rtss);
    2. copies your installed RivaTuner Statistics Server fresh and makes it measurement-only
       (Get-RecallRtssProfileEdits: no OSD, no framerate limit, no scanline sync, no Reflex
       injection; 1% low kept current); RTSS's own binaries are copied unmodified (its hook
       DLL's signature matters to anti-cheat);
    3. registers two on-demand tasks with highest privileges, no trigger:
         "Recall RTSS start"  %ProgramFiles%\Recall\rtss\RTSS.exe (interactive: RTSS needs the desktop)
         "Recall RTSS stop"   hidden PowerShell (S4U: no window); its command is inline in the
                              task (no script file), and stops only RTSS under that folder.
  Your own "RTSS" / "MSIAfterburner" tasks and your RTSS in Program Files (x86) are never changed.
#>

$script:RecallRtssSource = 'C:\Program Files (x86)\RivaTuner Statistics Server'
$script:RecallRtssStartTask = 'Recall RTSS start'
$script:RecallRtssStopTask = 'Recall RTSS stop'
$script:RecallRtssScript = $PSCommandPath
# Bump when Get-RecallRtssProfileEdits changes, so a reinstall re-applies the edits.
$script:RecallRtssRevision = 2
$script:RecallRtssMarker = 'recall-rtss.txt'

# %ProgramFiles%\Recall\rtss (the 64-bit Program Files, also from a 32-bit shell).
function Get-RecallRtssDir {
  $pf = if ($env:ProgramW6432) { $env:ProgramW6432 } else { $env:ProgramFiles }
  Join-Path $pf 'Recall\rtss'
}

# Set Key=Value under [Section] of an INI file, keeping every other line byte for byte (Latin-1
# round trip), the newline style and a trailing newline. Every Key line in the section is
# replaced; a missing key is added at the end of the section; a missing section is appended.
function Set-RecallIniKey {
  param([string]$Path, [string]$Section, [string]$Key, [string]$Value)
  $enc = [Text.Encoding]::GetEncoding(28591)
  $text = if (Test-Path -LiteralPath $Path) { [IO.File]::ReadAllText($Path, $enc) } else { '' }
  $nl = if ($text -match "(?<!`r)`n") { "`n" } else { "`r`n" }
  $trailing = ($text.Length -eq 0) -or $text.EndsWith("`n")
  $lines = [Collections.Generic.List[string]]::new()
  if ($text.Length) { $lines.AddRange([string[]]($text -split "`r?`n")) }
  if ($text.Length -and $trailing) { $lines.RemoveAt($lines.Count - 1) }
  $entry = "$Key=$Value"
  $keyPattern = '^' + [regex]::Escape($Key) + '\s*='
  $inSection = $false; $seen = $false; $found = $false; $insertAt = -1
  for ($i = 0; $i -lt $lines.Count; $i++) {
    $t = $lines[$i].Trim()
    if ($t -match '^\[(.+)\]$') {
      if ($inSection -and $insertAt -lt 0) { $insertAt = $i }
      $inSection = ($Matches[1].Trim() -ieq $Section)
      if ($inSection) { $seen = $true }
      continue
    }
    if ($inSection -and $t -match $keyPattern) { $lines[$i] = $entry; $found = $true }
  }
  if (-not $found) {
    if ($insertAt -ge 0) { $lines.Insert($insertAt, $entry) }
    elseif ($seen) { $lines.Add($entry) }
    else { $lines.Add("[$Section]"); $lines.Add($entry) }
  }
  $out = ($lines -join $nl) + $(if ($trailing) { $nl } else { '' })
  [IO.File]::WriteAllText($Path, $out, $enc)
}

# Measurement only: Recall's RTSS must not change how games run. Applied to Profiles\Global and
# every Profiles\*.cfg (game profiles override Global, so each gets the same values). Key names
# and meanings: the RTSS 7.3 profiles, the key list RTSS.exe / RTSSHooks64.dll read, and RTSS's
# Help\ context help. Keys a profile does not set fall back to Global; the shipped
# ProfileTemplates only set compatibility keys (EnableHooking, CoordinateSpace, ...), so a
# template RTSS instantiates later inherits these values. Left alone: [Hooking] (needed to
# measure), PassiveWait* (limiter wait method, inert without a limit), [Shared] Flags in Config.
function Get-RecallRtssProfileEdits {
  @(
    # -- Nothing drawn in the game (Recall's own island shows the numbers).
    @('OSD', 'EnableOSD', '0'),              # the on-screen display itself
    @('OSD', 'EnableStat', '0'),             # "Show own statistics": framerate / benchmark text in the OSD
    @('OSD', 'EnableFrameColorBar', '0'),    # FCAT frame colour bar overlay
    @('OSD', 'EnableFrametimeHistory', '0'), # frametime graph overlay
    # -- No framerate limiter (your profiles cap at 480 FPS).
    @('Framerate', 'Limit', '0'),                # framerate limit; help: "Set it to zero to disable the limit"
    @('Framerate', 'LimitDenominator', '1'),     # fractional-limit denominator; 1 = neutral
    @('Framerate', 'LimitTime', '0'),            # frametime-based limit (the alternate limit mode); 0 = none
    @('Framerate', 'LimitTimeDenominator', '1'), # its denominator; 1 = neutral
    @('Framerate', 'SyncLimiter', '0'),          # limiter mode: 0 = Async; never "NVIDIA Reflex" (turns on Reflex low latency)
    # -- No scanline sync (help: "Set it to zero to disable the synchronization").
    @('Framerate', 'SyncScanline0', '0'),        # target scanline
    @('Framerate', 'SyncScanline1', '0'),        # second target scanline (double sync)
    @('Framerate', 'SyncPeriods', '0'),          # single / double / half sync multiplier
    @('Framerate', 'SyncDisplay', '0'),          # display used for scanline sync (neutral)
    # -- No NVIDIA Reflex injection into games.
    @('Framerate', 'ReflexSetLatencyMarker', '0'), # injects Reflex latency markers into D3D11/12 games without native Reflex (on in your profiles)
    @('Framerate', 'ReflexSleep', '0'),            # NvAPI_D3D_Sleep placement; only used in the Reflex limiter mode, excluded above
    @('Framerate', 'ReflexLowLatency', '0'),       # forced Reflex low-latency mode (key read by RTSS, absent from your profiles)
    @('Framerate', 'ReflexLowLatencyBoost', '0'),  # forced Reflex boost (same)
    # -- 1% low reflects the last few seconds (help: "ring mode ... if you permanently keep the
    #    benchmark mode enabled"); 0 = unlimited (whole session), 1 = ring.
    @('Statistics', 'PercentileBuffer', '1')
  )
}

function Set-RecallRtssMeasurementOnly {
  param([string]$RtssDir)
  $profiles = Join-Path $RtssDir 'Profiles'
  New-Item -ItemType Directory -Force -Path $profiles | Out-Null
  $targets = @(Join-Path $profiles 'Global') + @(Get-ChildItem -LiteralPath $profiles -Filter '*.cfg' -File | ForEach-Object { $_.FullName })
  foreach ($path in $targets) {
    foreach ($e in Get-RecallRtssProfileEdits) { Set-RecallIniKey -Path $path -Section $e[0] -Key $e[1] -Value $e[2] }
  }
  $targets
}

# The stop task's command (inline in the task): closes, then force-stops, only RTSS processes under $RtssDir.
function Get-RecallRtssStopCommand {
  param([string]$RtssDir)
  $body = @'
$dir = '__DIR__\'
$names = 'RTSS', 'RTSSHooksLoader64', 'RTSSHooksLoader', 'EncoderServer64', 'EncoderServer'
function Get-Ours { @(Get-Process -Name $names -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($dir, [StringComparison]::OrdinalIgnoreCase) }) }
$asked = $false
foreach ($p in (Get-Ours | Where-Object { $_.Name -eq 'RTSS' })) { if ($p.CloseMainWindow()) { $asked = $true } }
$deadline = (Get-Date).AddSeconds(3)
while ($asked -and (Get-Ours | Where-Object { $_.Name -eq 'RTSS' }) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
Get-Ours | Stop-Process -Force -ErrorAction SilentlyContinue
'@
  $body.Replace('__DIR__', $RtssDir.TrimEnd('\').Replace("'", "''"))
}

function Get-RecallRtssStopDescription { param([string]$RtssDir) "Stops Recall's private RTSS in $RtssDir (run by the Recall recorder)." }

function Get-RecallRtssVersion { param([string]$Dir) $exe = Join-Path $Dir 'RTSS.exe'; if (Test-Path -LiteralPath $exe) { (Get-Item -LiteralPath $exe).VersionInfo.FileVersion } }

# Both tasks exist and point at this copy.
function Test-RecallRtssTasks {
  param([string]$RtssDir)
  $start = Get-ScheduledTask -TaskName $script:RecallRtssStartTask -ErrorAction SilentlyContinue
  $stop = Get-ScheduledTask -TaskName $script:RecallRtssStopTask -ErrorAction SilentlyContinue
  $exe = Join-Path $RtssDir 'RTSS.exe'
  [bool]($start -and $stop -and ($start.Actions | Where-Object { $_.Execute -ieq $exe }) -and $stop.Description -eq (Get-RecallRtssStopDescription $RtssDir))
}

# The copy is current: same RTSS version as the source, same profile-edit revision.
function Test-RecallRtssCopy {
  param([string]$Source, [string]$RtssDir)
  $marker = Join-Path $RtssDir $script:RecallRtssMarker
  $have = Get-RecallRtssVersion $RtssDir
  if (-not $have -or $have -ne (Get-RecallRtssVersion $Source) -or -not (Test-Path -LiteralPath $marker)) { return $false }
  [bool](Select-String -LiteralPath $marker -SimpleMatch "revision=$script:RecallRtssRevision" -Quiet)
}

# Elevated: stop Recall's RTSS, copy fresh beside the old copy, make it measurement-only, swap
# (a copy in use is left whole), then register the tasks. Returns 0 / 1; errors go to $LogPath.
function Invoke-RecallRtssElevated {
  param([string]$Source, [string]$RtssDir, [string]$User, [string]$LogPath, [switch]$NoTasks)
  try {
    $ErrorActionPreference = 'Stop'
    & ([ScriptBlock]::Create((Get-RecallRtssStopCommand $RtssDir)))
    $staging = "$RtssDir.new"; $old = "$RtssDir.old"
    New-Item -ItemType Directory -Force -Path (Split-Path $RtssDir -Parent) | Out-Null
    foreach ($stale in @($staging, $old)) { if (Test-Path -LiteralPath $stale) { Remove-Item -LiteralPath $stale -Recurse -Force } }
    Copy-Item -LiteralPath $Source -Destination $staging -Recurse -Force
    Set-RecallRtssMeasurementOnly $staging | Out-Null
    [IO.File]::WriteAllText((Join-Path $staging $script:RecallRtssMarker), "revision=$script:RecallRtssRevision`r`nsource_version=$(Get-RecallRtssVersion $Source)`r`n")
    if (Test-Path -LiteralPath $RtssDir) { Rename-Item -LiteralPath $RtssDir -NewName (Split-Path $old -Leaf) }
    Rename-Item -LiteralPath $staging -NewName (Split-Path $RtssDir -Leaf)
    if (Test-Path -LiteralPath $old) { Remove-Item -LiteralPath $old -Recurse -Force -ErrorAction SilentlyContinue }
    if ($NoTasks) { return 0 }
    $stopB64 = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes((Get-RecallRtssStopCommand $RtssDir)))
    $start = New-ScheduledTaskAction -Execute (Join-Path $RtssDir 'RTSS.exe') -WorkingDirectory $RtssDir
    $stop = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -Argument "-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand $stopB64"
    $desktop = New-ScheduledTaskPrincipal -UserId $User -LogonType Interactive -RunLevel Highest
    $hidden = New-ScheduledTaskPrincipal -UserId $User -LogonType S4U -RunLevel Highest
    $long = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
    $short = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName $script:RecallRtssStartTask -Action $start -Principal $desktop -Settings $long -Force `
      -Description "Starts Recall's private RTSS (run by the Recall recorder when performance stats turn on)." | Out-Null
    Register-ScheduledTask -TaskName $script:RecallRtssStopTask -Action $stop -Principal $hidden -Settings $short -Force `
      -Description (Get-RecallRtssStopDescription $RtssDir) | Out-Null
    return 0
  } catch {
    "$($_.Exception.Message)" | Out-File -LiteralPath $LogPath -Encoding utf8
    return 1
  }
}

# The one line the administrator prompt runs: load this file, run the elevated step, exit with its code.
function Get-RecallRtssElevatedCommand {
  param([string]$Source, [string]$RtssDir, [string]$User, [string]$LogPath)
  $q = { param($s) "'" + $s.Replace("'", "''") + "'" }
  ". $(& $q $script:RecallRtssScript); `$code = Invoke-RecallRtssElevated -Source $(& $q $Source) -RtssDir $(& $q $RtssDir) -User $(& $q $User) -LogPath $(& $q $LogPath); exit ([int](`$code | Select-Object -Last 1))"
}

function Install-RecallRtss {
  param([string]$InstallDir, [string]$Repo)
  # The earlier layout kept a user-writable copy in the install folder: remove it (regenerable).
  $legacy = Join-Path $InstallDir 'rtss'
  if (Test-Path -LiteralPath (Join-Path $legacy 'RTSS.exe')) {
    Remove-Item -LiteralPath $legacy -Recurse -Force
    "Removed the old user-writable RTSS copy at $legacy."
  }
  $source = $script:RecallRtssSource
  if (-not (Test-Path -LiteralPath (Join-Path $source 'RTSS.exe'))) {
    Write-Warning "RTSS is not installed ($source): performance stats show '-' for FPS. Install RivaTuner Statistics Server, then re-run."
    return
  }
  $dest = Get-RecallRtssDir
  foreach ($forbidden in @($env:LOCALAPPDATA, $Repo) | Where-Object { $_ }) {
    if ($dest.StartsWith([IO.Path]::GetFullPath($forbidden).TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
      throw "refusing to place RTSS in a user-writable folder ($dest)"
    }
  }
  if ((Test-RecallRtssCopy $source $dest) -and (Test-RecallRtssTasks $dest)) {
    "RTSS copy in $dest is current (version $(Get-RecallRtssVersion $dest)); no administrator prompt needed."
    return
  }
  $user = [Security.Principal.WindowsIdentity]::GetCurrent().Name
  $log = Join-Path $env:TEMP "recall-rtss-$PID.log"
  $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes((Get-RecallRtssElevatedCommand $source $dest $user $log)))
  "Setting up Recall's RTSS in ${dest}: approve the one administrator prompt (RTSS needs administrator rights)."
  try {
    $p = Start-Process -FilePath 'powershell.exe' -Verb RunAs -WindowStyle Hidden -Wait -PassThru `
      -ArgumentList @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $encoded)
  } catch {
    Write-Warning 'Administrator approval was declined: performance stats show "-" for FPS until you re-run the installer and approve it (or pass -NoRtss).'
    return
  }
  $why = if (Test-Path -LiteralPath $log) { (Get-Content -LiteralPath $log -Raw).Trim() } else { "exit code $($p.ExitCode)" }
  Remove-Item -LiteralPath $log -ErrorAction SilentlyContinue
  if ($p.ExitCode -eq 0 -and (Test-RecallRtssCopy $source $dest) -and (Test-RecallRtssTasks $dest)) {
    "Recall's RTSS is ready in $dest (measurement only; tasks '$script:RecallRtssStartTask', '$script:RecallRtssStopTask')."
  } else { Write-Warning "Recall's RTSS was not set up ($why); Settings will say 'RTSS not set up'." }
}
