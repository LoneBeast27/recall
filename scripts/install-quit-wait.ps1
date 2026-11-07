# Dot-sourced by install.ps1 (step 2): the wait on a quit sent to a recorder that may be
# finishing a clip (audit R5-startup-1). Uses install.ps1's Get-Installed, Get-Controller and
# Get-PipeServer; never run on its own.
# The recorder that got the quit still runs: same pid and start time (a reused pid is not it).
function Test-QuitPidAlive($quit) {
  [bool](Get-CimInstance Win32_Process -Filter "ProcessId = $($quit.pid)" | Where-Object { $null -eq $quit.created -or $_.CreationDate -eq $quit.created })
}
# Its status, read from the pipe the quit went to; $null when it does not answer within 2 s.
# Only the recorder that got the quit is read (its account was checked when the quit was sent).
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
# One progress line: what the recorder says it is doing, else that the run waits for it.
function Format-QuitProgress($quit, [datetime]$since) {
  $mins = [int][math]::Floor(((Get-Date) - $since).TotalMinutes)
  $s = Get-RecorderStatus $quit
  if ($s -and $s.finishing_clip) {
    $p = $s.finishing_progress
    $what = if ($p -and $p.bytes) { "$([math]::Round($p.bytes / 1MB)) MB written" } elseif ($p -and $p.saves) { "$($p.saves) clips still being written" } else { 'still being written' }
    "Recall is finishing a clip ($what, $mins min so far); waiting. Ctrl+C stops the installer; Recall keeps saving and then quits."
  } else {
    "Waiting for Recall (pid $($quit.pid)) to exit ($mins min so far)... Ctrl+C stops the installer."
  }
}
# A delivered quit can wait for a clip being saved: the controller waits at least 60 s and on
# while the file grows, with no bound it shares with us (fix/deep-overlay). So: 8 s for
# everything this run quits; then, only when a quit reached a recorder, as long as THAT recorder
# runs (no cap, one line a minute; Ctrl+C is the way out); then 8 s more for its engine and
# overlay. A recorder that never got the quit gets the 8 s only (Stop-Recorder names it).
function Wait-ForQuit($quit) {
  $deadline = (Get-Date).AddSeconds(8)
  while ((Get-Date) -lt $deadline -and ((Get-Installed) -or (Get-Controller))) { Start-Sleep -Milliseconds 300 }
  if (-not $quit -or -not (Test-QuitPidAlive $quit)) { return }
  $since = $deadline.AddSeconds(-8); $next = Get-Date
  while (Test-QuitPidAlive $quit) {
    if ((Get-Date) -ge $next) { Write-Host (Format-QuitProgress $quit $since); $next = (Get-Date).AddMinutes(1) }
    Start-Sleep -Seconds 1
  }
  $deadline = (Get-Date).AddSeconds(8)
  while ((Get-Date) -lt $deadline -and ((Get-Installed) -or (Get-Controller))) { Start-Sleep -Milliseconds 300 }
}
