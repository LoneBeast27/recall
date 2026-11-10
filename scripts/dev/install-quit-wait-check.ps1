# Proves the quit and wait decisions in scripts/install-quit-wait.ps1 with every outside call stubbed:
# no process, pipe, WMI query, file or registry is touched, and the clock is virtual (Get-Date and
# Start-Sleep are stubs), so a 30-minute save runs in well under a second. Checks: no second quit
# while a recorder is finishing a clip (R6-startup-1); the long wait follows finishing_clip with
# 2 min of grace and the -QuitWaitMinutes cap (R6-startup-2); CIM errors are retried and never
# escape (R6-startup-3); "1 clip" (R6-startup-6). Prints PASS/FAIL; exit 0 = all PASS.
#   powershell -NoProfile -File scripts\dev\install-quit-wait-check.ps1 [-WaitScript <path>]
param([string]$WaitScript = (Join-Path (Split-Path $PSScriptRoot -Parent) 'install-quit-wait.ps1'))
$ErrorActionPreference = 'Stop'
$fail = 0
function Say([bool]$ok, [string]$what) { "$(if ($ok) { 'PASS' } else { 'FAIL' }) quit wait: $what"; if (-not $ok) { $script:fail++ } }

# --- stubs (functions win over cmdlets of the same name) ---
$script:t0 = [datetime]'2026-01-01T00:00:00'; $script:now = $script:t0
function Get-Date { $script:now }
function Start-Sleep([int]$Seconds = 0, [int]$Milliseconds = 0) {
  $script:now = $script:now.AddSeconds($Seconds).AddMilliseconds($Milliseconds)
  # A wait that never ends (the bug this checks for) stops at 10 virtual hours instead of hanging.
  if (($script:now - $script:t0).TotalHours -gt 10) { throw 'runaway: still waiting after 10 virtual hours' }
}
function Write-Host($Object) { $script:printed += @([string]$Object) }
function Elapsed { ($script:now - $script:t0).TotalSeconds }
# The process list: $script:cim runs per query (it can throw, like a WMI hiccup).
function Get-CimInstance($ClassName, $Filter) { $script:cimCalls++; & $script:cim }
function Get-Installed { & $script:installed }
function Get-Controller { $script:controllers }
function Get-ProcessUserSid([int]$id) { $script:sids[$id] }
function Send-Recorder([string]$line, [string]$name = $pipeName, [int]$tries = 5) {
  $script:sends += @("$name $line"); $script:sentTo = @{ pid = 200; created = 'c200'; pipe = $name }; $true
}
function Get-PipeServer { throw 'Get-PipeServer must not run in the check' }
$pipeName = 'stub-pipe-session'; $legacyPipe = 'stub-pipe-legacy'; $mySid = 'S-me'

. $WaitScript
# Overrides the real pipe read defined above: answers from $script:status (a scriptblock on the clock).
function Get-RecorderStatus($quit) { $script:statusReads++; & $script:status }

function Reset {
  $script:now = $script:t0; $script:printed = @(); $script:sends = @(); $script:statusReads = 0; $script:cimCalls = 0
  $script:controllers = @(); $script:sids = @{}; $script:installed = { $null }
  $script:cim = { $null }; $script:status = { $null }
}
$proc = [pscustomobject]@{ ProcessId = 100; CreationDate = 'c100' }
$quit = @{ pid = 100; created = 'c100'; pipe = $pipeName }
function Run-Wait($cap) { try { Wait-ForQuit $quit $cap; $null } catch { $_.Exception.Message } }
# Each case runs on its own: one that throws (say, a function the script lacks) fails alone.
function Case([string]$name, [scriptblock]$body) {
  try { & $body } catch { Say $false "$name threw: $($_.Exception.Message)" }
}

# 1. R6-startup-1: a controller already finishing a clip gets no second quit; the run waits on it.
Case 'case 1' {
    Reset; $script:controllers = @($proc); $script:sids = @{ 100 = 'S-me' }
    $script:status = { [pscustomobject]@{ finishing_clip = $true } }
    $q = Request-RecorderQuit
    Say ($script:sends.Count -eq 0) "no quit is sent while the recorder is finishing a clip (sent: $($script:sends -join '; '))"
    Say ($q -and $q.pid -eq 100 -and $q.created -eq 'c100' -and $q.pipe -eq $pipeName) "the run waits on that recorder (pid $($q.pid), pipe $($q.pipe))"
    Say ([bool]($script:printed -like '*already quitting*no second quit sent*')) "it says why no quit was sent"
}

# 1b. Item 10: a recorder finishing a clip because a sign-out is held (its quit is the sign-out's
#     alone) gets one plain quit: a cancelled sign-out calls its own quit off, and the plain quit
#     joins the wait and survives the cancel (record_quit_wait::on_quit_request keeps it).
Case 'case 1b' {
    Reset; $script:controllers = @($proc); $script:sids = @{ 100 = 'S-me' }
    $script:status = { [pscustomobject]@{ finishing_clip = $true; quit_for_sign_out = $true } }
    $q = Request-RecorderQuit
    Say ($script:sends.Count -eq 1 -and $script:sends[0] -eq "$pipeName {""cmd"":""quit""}") "a held sign-out gets one plain quit ($($script:sends -join '; '))"
    Say ($q.pid -eq 200) "the wait follows the recorder that got the quit (pid $($q.pid))"
}

# 2. Not finishing: one quit, on the session pipe, and the run waits on the recorder that got it.
Case 'case 2' {
    Reset; $script:controllers = @($proc); $script:sids = @{ 100 = 'S-me' }
    $script:status = { [pscustomobject]@{ finishing_clip = $false } }
    $q = Request-RecorderQuit
    Say ($script:sends.Count -eq 1 -and $script:sends[0] -eq "$pipeName {""cmd"":""quit""}") "one quit on the session pipe ($($script:sends -join '; '))"
    Say ($q.pid -eq 200) "the wait follows the recorder that got the quit (pid $($q.pid))"
}

# 3. Another account's controller is never read; the quit path (with its own owner refusal) runs.
Case 'case 3' {
    Reset; $script:controllers = @($proc); $script:sids = @{ 100 = 'S-other' }
    $script:status = { [pscustomobject]@{ finishing_clip = $true } }
    $q = Request-RecorderQuit
    Say ($script:statusReads -eq 0 -and $script:sends.Count -eq 1) "another account's recorder is not read (reads $($script:statusReads), sends $($script:sends.Count))"
}

# 4. No controller: nothing sent, nothing to wait on.
Case 'case 4' {
    Reset
    $q = Request-RecorderQuit
    Say ($null -eq $q -and $script:sends.Count -eq 0) 'no controller: no quit, no wait'
}

# 5. A 30-minute save: waited for to the end, one line a minute, no throw.
Case 'case 5' {
    Reset; $script:cim = { if ((Elapsed) -lt 1800) { $proc } }
    $script:status = { [pscustomobject]@{ finishing_clip = $true; finishing_progress = [pscustomobject]@{ saves = 1 } } }
    $err = Run-Wait 0; $e = Elapsed
    Say ($null -eq $err -and $e -ge 1800 -and $e -lt 1815) "a 30 min save is waited for to the end ($([int]$e) s, error: $err)"
    Say ($script:printed.Count -ge 29 -and $script:printed.Count -le 32) "one progress line a minute ($($script:printed.Count) lines)"
    Say ([bool]($script:printed[0] -like '*(1 clip still being written,*')) "one save reads '1 clip' ($($script:printed[0]))"
    Say ($script:statusReads -ge 800 -and $script:statusReads -le 950) "status polled about every 2 s ($($script:statusReads) reads in $([int]$e) s)"
}

# 6. Alive but not finishing a clip: 2 min of grace, then a throw naming the pid.
Case 'case 6' {
    Reset; $script:cim = { $proc }; $script:status = { [pscustomobject]@{ finishing_clip = $false } }
    $err = Run-Wait 0; $e = Elapsed
    Say ($err -like '*pid 100*says it is not finishing a clip*' -and $e -ge 120 -and $e -le 135) "not finishing: throws after the 2 min grace ($([int]$e) s: $err)"
}

# 7. Alive but not answering on its pipe: the same grace, and the message says so.
Case 'case 7' {
    Reset; $script:cim = { $proc }; $script:status = { $null }
    $err = Run-Wait 0; $e = Elapsed
    Say ($err -like '*pid 100*has not answered*' -and $e -ge 120 -and $e -le 135) "no answer: throws after the 2 min grace ($([int]$e) s)"
}

# 8. Finishing for 10 min, then not (but still alive): the grace starts when it turns false.
Case 'case 8' {
    Reset; $script:cim = { $proc }; $script:status = { [pscustomobject]@{ finishing_clip = ((Elapsed) -lt 600) } }
    $err = Run-Wait 0; $e = Elapsed
    Say ($null -ne $err -and $e -ge 720 -and $e -le 735) "grace counts from the end of finishing_clip ($([int]$e) s)"
}

# 9. -QuitWaitMinutes 5 caps a save that never ends.
Case 'case 9' {
    Reset; $script:cim = { $proc }; $script:status = { [pscustomobject]@{ finishing_clip = $true } }
    $err = Run-Wait 5; $e = Elapsed
    Say ($err -like '*pid 100*-QuitWaitMinutes 5*' -and $e -ge 300 -and $e -le 305) "-QuitWaitMinutes 5 ends the wait at 5 min ($([int]$e) s: $err)"
}

# 10. Grace resets when finishing_clip comes back (a status read that briefly failed).
Case 'case 10' {
    Reset
    $w = New-QuitWait $script:t0 0
    $a = Get-QuitWaitStep $w $script:t0.AddSeconds(10) $true $null
    $b = Get-QuitWaitStep $w $script:t0.AddSeconds(100) $true $true
    $c = Get-QuitWaitStep $w $script:t0.AddSeconds(200) $true $true
    $d = Get-QuitWaitStep $w $script:t0.AddSeconds(201) $false $true
    Say ("$a $b $c $d" -eq 'wait wait wait exited' -and $null -eq $w.graceFrom) "a returning finishing_clip clears the grace ($a $b $c $d)"
}

# 11. R6-startup-3: CIM errors are retried; three in a row count as still running, never a raw throw.
Case 'case 11' {
    Reset; $script:cimN = 0; $script:cim = { $script:cimN++; if ($script:cimN -le 2) { throw 'WMI hiccup' }; $proc }
    $r = $null; $x = $null; try { $r = Test-QuitPidAlive $quit } catch { $x = $_.Exception.Message }
    Say ($r -eq $true -and $null -eq $x -and $script:cimCalls -eq 3) "two failed queries then an answer: alive, no throw ($r, $script:cimCalls calls, $x)"
    Reset; $script:cim = { throw 'WMI down' }
    $r = $null; $x = $null; try { $r = Test-QuitPidAlive $quit } catch { $x = $_.Exception.Message }
    Say ($r -eq $true -and $null -eq $x -and $script:cimCalls -eq 3) "three failed queries: counted alive, no throw ($r, $script:cimCalls calls, $x)"
    Reset; $script:installed = { throw 'WMI down' }
    $r = $null; $x = $null; try { $r = Test-AnyRecorder } catch { $x = $_.Exception.Message }
    Say ($r -eq $true -and $null -eq $x) "the 8 s wait's query failing counts as still running, no throw ($r, $x)"
    Reset; $script:cim = { throw 'WMI down' }; $script:status = { $null }
    $err = Run-Wait 0
    Say ($err -like '*pid 100*has not answered*') "WMI down for the whole wait: the run ends on the grace with the pid named ($err)"
    Reset; $script:controllers = $null
    function Get-Controller { throw 'WMI down' }
    $x = $null; try { $q = Request-RecorderQuit } catch { $x = $_.Exception.Message }
    # Not a check: before any quit, a failing query still stops the run (as before this change).
    "INFO quit wait: Request-RecorderQuit with Get-Controller failing $(if ($x) { "throws '$x' before any quit is sent" } else { 'does not throw' })"
    function Get-Controller { $script:controllers }
}

# 12. Nothing delivered: 8 s only, even while the recorder runs.
Case 'case 12' {
    Reset; $script:cim = { $proc }; $script:installed = { $proc }
    try { Wait-ForQuit $null 0 } catch { }
    Say ((Elapsed) -ge 8 -and (Elapsed) -lt 9 -and $script:cimCalls -eq 0) "no quit delivered: the 8 s only, the pid is never polled ($([int](Elapsed)) s)"
}

# 13. Progress text: clip counts.
Case 'case 13' {
    $two = Format-QuitProgress $quit $script:t0 ([pscustomobject]@{ finishing_clip = $true; finishing_progress = [pscustomobject]@{ saves = 2 } })
    $mb = Format-QuitProgress $quit $script:t0 ([pscustomobject]@{ finishing_clip = $true; finishing_progress = [pscustomobject]@{ saves = 1; bytes = 50MB } })
    $no = Format-QuitProgress $quit $script:t0 $null
    Say ($two -like '*(2 clips still being written,*' -and $mb -like '*(50 MB written,*' -and $no -like 'Waiting for Recall (pid 100)*') "progress text: '2 clips', MB, and the plain wait line"
}
if ($fail) { "FAIL: $fail check(s)"; exit 1 } else { 'PASS: all quit-wait checks'; exit 0 }
