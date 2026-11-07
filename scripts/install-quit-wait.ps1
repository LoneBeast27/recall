# Dot-sourced by install.ps1 (step 2): the quit sent to a recorder that may be finishing a clip,
# and the wait on it (audits R5-startup-1, R6-startup-1..3). Uses install.ps1's Send-Recorder,
# Get-Installed, Get-Controller, Get-PipeServer, Get-ProcessUserSid, $pipeName, $legacyPipe and
# $mySid; never run on its own. dev\install-quit-wait-check.ps1 drives it with stubs.

# A WMI/CIM query, tried 3 times; when all 3 fail, $fallback (a hiccup never ends the run with a
# raw exception mid-wait, R6-startup-3).
function Invoke-CimRetry([scriptblock]$query, $fallback) {
  for ($try = 1; $try -le 3; $try++) {
    try { return (& $query) } catch { if ($try -lt 3) { Start-Sleep -Milliseconds 500 } }
  }
  $fallback
}
# The recorder that got the quit still runs: same pid and start time (a reused pid is not it).
# A query that keeps failing counts as still running: the grace and -QuitWaitMinutes still end the wait.
function Test-QuitPidAlive($quit) {
  Invoke-CimRetry { [bool](Get-CimInstance Win32_Process -Filter "ProcessId = $($quit.pid)" | Where-Object { $null -eq $quit.created -or $_.CreationDate -eq $quit.created }) } $true
}
# Anything this run quits still runs (the 8 s waits). A failing query counts as yes.
function Test-AnyRecorder { Invoke-CimRetry { [bool]((Get-Installed) -or (Get-Controller)) } $true }
# Its status, read from the pipe the quit went to; $null when it does not answer within 2 s.
# Only the recorder named by $quit is read: the pipe's server must be that pid.
function Get-RecorderStatus($quit) {
  $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', $quit.pipe, [IO.Pipes.PipeDirection]::InOut, [IO.Pipes.PipeOptions]::Asynchronous)
  try {
    $pipe.Connect(1000)
    if ((Get-PipeServer $pipe) -ne $quit.pid) { return $null }
    # The hub answers a new client with its status unasked; ask anyway (an older build).
    $w = [IO.StreamWriter]::new($pipe); $w.AutoFlush = $true; $w.WriteLine('{"cmd":"status"}')
    $reader = [IO.StreamReader]::new($pipe); $until = (Get-Date).AddSeconds(2); $read = $null
    while ((Get-Date) -lt $until) {
      # One read in flight at a time; the dispose below cancels one still pending.
      if (-not $read) { $read = $reader.ReadLineAsync() }
      if (-not $read.Wait(250)) { continue }
      $line = $read.Result; $read = $null
      if ($null -eq $line) { break }
      $m = $null; try { $m = $line | ConvertFrom-Json } catch { }
      if ($m -and $m.mod -eq 'session' -and $m.event.type -eq 'status') { return $m.event }
    }
    $null
  } catch { $null } finally { $pipe.Dispose() }
}
# A controller of ours (this account) that already says finishing_clip got its quit from an earlier
# run: a second quit could force it and lose the clip (R6-startup-1). Only this build's session
# pipe is asked; a pre-session recorder has no finishing_clip.
function Get-PendingQuit {
  foreach ($c in @(Invoke-CimRetry { @(Get-Controller) } @())) {
    if ((Get-ProcessUserSid ([int]$c.ProcessId)) -ne $mySid) { continue }
    $q = @{ pid = [int]$c.ProcessId; created = $c.CreationDate; pipe = $pipeName }
    $s = Get-RecorderStatus $q
    if ($s -and $s.finishing_clip -eq $true) { return $q }
  }
  $null
}
# Asks the recorder to quit, once: none is sent while one is already finishing a clip. Returns the
# recorder to wait on (the one already quitting, or the one that got this quit), else $null.
function Request-RecorderQuit {
  $pending = Get-PendingQuit
  if ($pending) {
    Write-Host "Recall (pid $($pending.pid)) is already quitting and finishing a clip (an earlier run asked); no second quit sent, waiting for it."
    return $pending
  }
  $sent = $false
  if (Get-Controller) {
    # Either pipe answering counts: this build's session pipe first, then the old bare name.
    $sent = Send-Recorder '{"cmd":"quit"}'
    if (-not $sent -and (Get-Controller)) { $sent = Send-Recorder '{"cmd":"quit"}' $legacyPipe }
  }
  if ($sent) { $script:sentTo } else { $null }
}
# One progress line from the last status read: what the recorder says it is doing, else that the run waits for it.
function Format-QuitProgress($quit, [datetime]$since, $s) {
  $mins = [int][math]::Floor(((Get-Date) - $since).TotalMinutes)
  if ($s -and $s.finishing_clip) {
    $p = $s.finishing_progress
    $what = if ($p -and $p.bytes) { "$([math]::Round($p.bytes / 1MB)) MB written" } elseif ($p -and $p.saves -eq 1) { '1 clip still being written' } elseif ($p -and $p.saves) { "$($p.saves) clips still being written" } else { 'still being written' }
    "Recall is finishing a clip ($what, $mins min so far); waiting. Ctrl+C stops the installer; Recall keeps saving and then quits (a re-run waits for it too)."
  } else {
    "Waiting for Recall (pid $($quit.pid)) to exit ($mins min so far)... Ctrl+C stops the installer."
  }
}
# The long wait's state: 2 min of grace once the recorder is not finishing a clip (or not answering),
# and the -QuitWaitMinutes cap (0 = none) counted from $since.
function New-QuitWait([datetime]$since, [int]$capMinutes) {
  @{ since = $since; grace = [timespan]::FromMinutes(2); graceFrom = $null; silent = $false
     minutes = $capMinutes; cap = $(if ($capMinutes -gt 0) { $since.AddMinutes($capMinutes) } else { $null }); why = $null }
}
# One decision per poll (no I/O). $finishing: $true / $false from the status, $null = no answer.
# 'exited', 'wait' or 'timeout' ($w.why says which limit).
function Get-QuitWaitStep($w, [datetime]$now, [bool]$alive, $finishing) {
  if (-not $alive) { return 'exited' }
  if ($finishing -eq $true) { $w.graceFrom = $null } elseif (-not $w.graceFrom) { $w.graceFrom = $now }
  if ($finishing -ne $true) { $w.silent = ($null -eq $finishing) }
  if ($w.cap -and $now -ge $w.cap) { $w.why = 'cap'; return 'timeout' }
  if ($w.graceFrom -and ($now - $w.graceFrom) -ge $w.grace) { $w.why = 'grace'; return 'timeout' }
  'wait'
}
function Format-QuitTimeout($quit, $w) {
  if ($w.why -eq 'cap') {
    return "Recall (pid $($quit.pid)) got the quit but still runs after -QuitWaitMinutes $($w.minutes); it keeps saving and then quits. Nothing was replaced: re-run once it has quit (a re-run sends no second quit while it saves)."
  }
  $state = if ($w.silent) { 'has not answered on its pipe' } else { 'says it is not finishing a clip' }
  "Recall (pid $($quit.pid)) got the quit but has not exited, and for 2 min it $state. Nothing was replaced. Quit Recall from the tray, then re-run."
}
# A delivered quit can wait for a clip being saved: the controller waits at least 60 s and on while
# the file grows (fix/deep-overlay). So: 8 s for everything this run quits; then, only when a quit
# reached a recorder, while THAT recorder runs AND its status (read every 2 s) says finishing_clip,
# with 2 min of grace once it does not (or stops answering), one line a minute, and the optional
# -QuitWaitMinutes cap (R6-startup-2); past either limit, a throw naming the pid. Then 8 s more for
# its engine and overlay. A recorder that never got the quit gets the 8 s only (Stop-Recorder names it).
function Wait-ForQuit($quit, [int]$capMinutes = 0) {
  $since = Get-Date; $deadline = $since.AddSeconds(8)
  while ((Get-Date) -lt $deadline -and (Test-AnyRecorder)) { Start-Sleep -Milliseconds 300 }
  if (-not $quit -or -not (Test-QuitPidAlive $quit)) { return }
  $w = New-QuitWait $since $capMinutes; $next = Get-Date; $poll = Get-Date; $s = $null
  while ($true) {
    if ((Get-Date) -ge $poll) { $s = Get-RecorderStatus $quit; $poll = (Get-Date).AddSeconds(2) }
    $finishing = if ($s) { $s.finishing_clip -eq $true } else { $null }
    $step = Get-QuitWaitStep $w (Get-Date) (Test-QuitPidAlive $quit) $finishing
    if ($step -eq 'exited') { break }
    if ($step -eq 'timeout') { throw (Format-QuitTimeout $quit $w) }
    if ((Get-Date) -ge $next) { Write-Host (Format-QuitProgress $quit $since $s); $next = (Get-Date).AddMinutes(1) }
    Start-Sleep -Seconds 1
  }
  $deadline = (Get-Date).AddSeconds(8)
  while ((Get-Date) -lt $deadline -and (Test-AnyRecorder)) { Start-Sleep -Milliseconds 300 }
}
