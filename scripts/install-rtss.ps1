<#
  RTSS step of scripts\install.ps1 (dot-sourced; defines functions only, runs nothing).

  Install-RecallRtss copies your installed RivaTuner Statistics Server into <install>\rtss with
  its own on-screen display off, then registers the two on-demand scheduled tasks the recorder
  runs (schtasks /run, no prompt) through ONE administrator prompt:
    "Recall RTSS start"  <install>\rtss\RTSS.exe, highest privileges, interactive, no trigger
    "Recall RTSS stop"   hidden PowerShell (S4U: no window) that stops only RTSS processes
                         whose path is under <install>\rtss (and its hook loaders)
  Your own "RTSS" / "MSIAfterburner" tasks and your RTSS in Program Files are never changed;
  RTSS is never copied into the repo or dist. RTSS's own files are copied unmodified (its hook
  DLL's signature matters to anti-cheat); only the profile INI files get EnableOSD=0.
#>

$script:RecallRtssSource = 'C:\Program Files (x86)\RivaTuner Statistics Server'
$script:RecallRtssStartTask = 'Recall RTSS start'
$script:RecallRtssStopTask = 'Recall RTSS stop'

# Set Key=Value under [Section] of an INI file, keeping every other line byte for byte (Latin-1
# round trip), the newline style and a trailing newline. Every Key line in the section is
# replaced; a missing key is added at the end of the section; a missing section is appended.
function Set-RecallIniKey([string]$Path, [string]$Section, [string]$Key, [string]$Value) {
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

# OSD off in Profiles\Global (created if RTSS never wrote one) and every Profiles\*.cfg.
function Disable-RecallRtssOsd([string]$RtssDir) {
  $profiles = Join-Path $RtssDir 'Profiles'
  New-Item -ItemType Directory -Force -Path $profiles | Out-Null
  $targets = @(Join-Path $profiles 'Global') + @(Get-ChildItem -LiteralPath $profiles -Filter '*.cfg' -File | ForEach-Object { $_.FullName })
  foreach ($path in $targets) { Set-RecallIniKey -Path $path -Section 'OSD' -Key 'EnableOSD' -Value '0' }
  $targets
}

# The stop task's command: closes, then force-stops, only RTSS processes under $RtssDir.
function Get-RecallRtssStopCommand([string]$RtssDir) {
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

function Get-RecallRtssStopDescription([string]$RtssDir) { "Stops Recall's private RTSS in $RtssDir (run by the Recall recorder)." }

# The script run elevated: registers both tasks (overwriting only these two names); errors go to $LogPath.
function Get-RecallRtssTaskScript([string]$RtssDir, [string]$User, [string]$LogPath) {
  $q = { param($s) $s.Replace("'", "''") }
  $stop = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes((Get-RecallRtssStopCommand $RtssDir)))
  $body = @'
$ErrorActionPreference = 'Stop'
try {
  $start = New-ScheduledTaskAction -Execute '__EXE__' -WorkingDirectory '__DIR__'
  $stop = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -Argument '-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand __STOP__'
  $desktop = New-ScheduledTaskPrincipal -UserId '__USER__' -LogonType Interactive -RunLevel Highest
  $hidden = New-ScheduledTaskPrincipal -UserId '__USER__' -LogonType S4U -RunLevel Highest
  $long = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
  $short = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew
  Register-ScheduledTask -TaskName '__START_TASK__' -Action $start -Principal $desktop -Settings $long -Description 'Starts Recall''s private RTSS (run by the Recall recorder when performance stats turn on).' -Force | Out-Null
  Register-ScheduledTask -TaskName '__STOP_TASK__' -Action $stop -Principal $hidden -Settings $short -Description '__STOP_DESC__' -Force | Out-Null
  exit 0
} catch {
  "$($_.Exception.Message)" | Out-File -LiteralPath '__LOG__' -Encoding utf8
  exit 1
}
'@
  $body.Replace('__EXE__', (& $q (Join-Path $RtssDir 'RTSS.exe'))).Replace('__DIR__', (& $q $RtssDir)).Replace('__STOP__', $stop).
    Replace('__USER__', (& $q $User)).Replace('__START_TASK__', $script:RecallRtssStartTask).Replace('__STOP_TASK__', $script:RecallRtssStopTask).
    Replace('__STOP_DESC__', (& $q (Get-RecallRtssStopDescription $RtssDir))).Replace('__LOG__', (& $q $LogPath))
}

# Both tasks exist and point at this copy (then no administrator prompt is needed).
function Test-RecallRtssTasks([string]$RtssDir) {
  $start = Get-ScheduledTask -TaskName $script:RecallRtssStartTask -ErrorAction SilentlyContinue
  $stop = Get-ScheduledTask -TaskName $script:RecallRtssStopTask -ErrorAction SilentlyContinue
  $exe = Join-Path $RtssDir 'RTSS.exe'
  [bool]($start -and $stop -and ($start.Actions | Where-Object { $_.Execute -ieq $exe }) -and $stop.Description -eq (Get-RecallRtssStopDescription $RtssDir))
}

# Stop Recall's own RTSS (never the user's) so its files can be replaced.
function Stop-RecallRtss {
  if (-not (Get-ScheduledTask -TaskName $script:RecallRtssStopTask -ErrorAction SilentlyContinue)) { return }
  Start-ScheduledTask -TaskName $script:RecallRtssStopTask
  $deadline = (Get-Date).AddSeconds(8)
  do { Start-Sleep -Milliseconds 300; $task = Get-ScheduledTask -TaskName $script:RecallRtssStartTask -ErrorAction SilentlyContinue }
  while ($task -and $task.State -eq 'Running' -and (Get-Date) -lt $deadline)
}

function Install-RecallRtss([string]$InstallDir, [string]$Repo) {
  $source = $script:RecallRtssSource
  if (-not (Test-Path -LiteralPath (Join-Path $source 'RTSS.exe'))) {
    Write-Warning "RTSS is not installed ($source): performance stats show '-' for FPS. Install RivaTuner Statistics Server, then re-run."
    return
  }
  $dest = [IO.Path]::GetFullPath((Join-Path $InstallDir 'rtss'))
  $repoFull = [IO.Path]::GetFullPath($Repo).TrimEnd('\') + '\'
  if ($dest.StartsWith($repoFull, [StringComparison]::OrdinalIgnoreCase)) {
    Write-Warning "RTSS not copied: $dest is inside the repo (RTSS is never copied into the repo or dist)."
    return
  }
  # Fresh copy beside the old one, then swap: a copy in use is left whole, never half-deleted.
  $staging = "$dest.new"; $old = "$dest.old"
  Stop-RecallRtss
  foreach ($stale in @($staging, $old)) { if (Test-Path -LiteralPath $stale) { Remove-Item -LiteralPath $stale -Recurse -Force } }
  Copy-Item -LiteralPath $source -Destination $staging -Recurse -Force
  $edited = Disable-RecallRtssOsd $staging
  if (Test-Path -LiteralPath $dest) {
    try { Rename-Item -LiteralPath $dest -NewName (Split-Path $old -Leaf) }
    catch {
      Remove-Item -LiteralPath $staging -Recurse -Force
      throw "the current $dest is in use (quit RTSS started from it, then re-run)"
    }
  }
  Rename-Item -LiteralPath $staging -NewName (Split-Path $dest -Leaf)
  if (Test-Path -LiteralPath $old) { Remove-Item -LiteralPath $old -Recurse -Force -ErrorAction SilentlyContinue }
  "RTSS copied to $dest (OSD off in $($edited.Count) profile file(s))."

  if (Test-RecallRtssTasks $dest) { "RTSS tasks already registered ('$script:RecallRtssStartTask', '$script:RecallRtssStopTask')."; return }
  $user = [Security.Principal.WindowsIdentity]::GetCurrent().Name
  $log = Join-Path $env:TEMP "recall-rtss-tasks-$PID.log"
  $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes((Get-RecallRtssTaskScript $dest $user $log)))
  'Registering the RTSS tasks: approve the one administrator prompt (RTSS needs administrator rights).'
  try {
    $p = Start-Process -FilePath 'powershell.exe' -Verb RunAs -WindowStyle Hidden -Wait -PassThru `
      -ArgumentList @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $encoded)
  } catch {
    Write-Warning 'Administrator approval was declined: performance stats show "-" for FPS until you re-run the installer and approve it (or pass -NoRtss).'
    return
  }
  $why = if (Test-Path -LiteralPath $log) { (Get-Content -LiteralPath $log -Raw).Trim() } else { "exit code $($p.ExitCode)" }
  Remove-Item -LiteralPath $log -ErrorAction SilentlyContinue
  if ($p.ExitCode -eq 0 -and (Test-RecallRtssTasks $dest)) { "RTSS tasks registered ('$script:RecallRtssStartTask', '$script:RecallRtssStopTask')." }
  else { Write-Warning "RTSS tasks were not registered ($why); Settings will say 'RTSS not set up'." }
}
