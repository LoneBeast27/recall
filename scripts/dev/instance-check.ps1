<#
.SYNOPSIS
  Single-instance check for the library app (recall-native.exe), headless.

  Every instance runs offscreen (RECALL_NATIVE_OFFSCREEN: silent, no focus, outside every monitor) in a
  sandbox (own APPDATA, empty clip folder, own cache, no recorder) with its own guard name
  (RECALL_INSTANCE_TAG), so the person's own Recall is never touched. Only instances this script started
  are closed or killed.

  Cases: a second start exits 0 and leaves the first running; an untagged check instance takes no guard;
  a closed window frees the guard; a killed instance never blocks the next start.

  Run through the build gate:
    powershell -NoProfile -File <gate dir>\gated.ps1 none powershell -NoProfile -File scripts\dev\instance-check.ps1 -Exe <recall-native.exe>
#>
param([Parameter(Mandatory)][string]$Exe)
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path $Exe).Path
$work = Join-Path $env:TEMP "recall-instance-check-$PID"
New-Item -ItemType Directory -Force -Path "$work\appdata", "$work\clips", "$work\cache" | Out-Null
$tag = "check-$PID"
$script:mine = @()
$script:pass = 0; $script:fail = 0

function Start-App([string]$name, [switch]$Untagged) {
  $vars = @{
    RECALL_NATIVE_OFFSCREEN = '1'; RECALL_NO_CONTROLLER = '1'; RECALL_CPU_MASK = 'ffff0000'
    RECALL_CONTROL_PIPE = "recall-instance-check-$PID"; RECALL_OUT_DIR = "$work\clips"
    RECALL_FOLDERS = "$work\folders.json"; RECALL_LIBRARY_META = "$work\library.json"; RECALL_CACHE_DIR = "$work\cache"
    RECALL_INSTANCE_TAG = $(if ($Untagged) { $null } else { $tag })
  }
  $saved = @{}; foreach ($k in $vars.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
  $realAppData = $env:APPDATA; $env:APPDATA = "$work\appdata"
  try {
    $p = Start-Process -FilePath $Exe -WorkingDirectory (Split-Path $Exe) -NoNewWindow -PassThru `
      -RedirectStandardError "$work\$name.err" -RedirectStandardOutput "$work\$name.out"
    [void]$p.Handle
  } finally {
    $env:APPDATA = $realAppData
    foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
  }
  $script:mine += $p
  $p
}

function Wait-Window($p, [int]$seconds = 30) {
  $t = Get-Date
  while ((-not $p.MainWindowHandle -or $p.MainWindowHandle -eq 0) -and -not $p.HasExited -and ((Get-Date) - $t).TotalSeconds -lt $seconds) { Start-Sleep -Milliseconds 100; $p.Refresh() }
  $p.Refresh()
  -not $p.HasExited -and $p.MainWindowHandle -ne 0
}

function Close-App($p) {
  if (-not $p.HasExited) { [void]$p.CloseMainWindow(); [void]$p.WaitForExit(10000) }
  if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force; [void]$p.WaitForExit(5000) } # our own instance only
}

function Check([string]$what, [bool]$ok, [string]$detail = '') {
  if ($ok) { $script:pass++; "PASS $what" } else { $script:fail++; "FAIL $what $detail" }
}

try {
  # 1. The first instance opens its window.
  $a = Start-App 'first'
  Check 'first instance opens a window' (Wait-Window $a) "exit=$(if ($a.HasExited) { $a.ExitCode })"

  # 2. A second start exits 0 at once, shows the first, and the first keeps running.
  $t = Get-Date
  $b = Start-App 'second'
  $ended = $b.WaitForExit(15000)
  $ms = [int]((Get-Date) - $t).TotalMilliseconds
  $b.Refresh()
  $err = Get-Content "$work\second.err" -Raw -ErrorAction SilentlyContinue
  Check 'second start exits' $ended "still running after 15 s"
  Check 'second start exits with code 0' ($ended -and $b.ExitCode -eq 0) "exit=$($b.ExitCode)"
  Check 'second start found the running window' ($err -match 'second start: showed the running window' -and $err -match "`"pid`":$($a.Id)") "stderr: $err"
  Check 'second start handed off at once (no 5 s search)' ($ms -lt 4000) "$ms ms"
  Check 'first instance still running' (-not $a.HasExited)
  "  second start took $ms ms"

  # 3. A check instance without a tag takes no guard: it runs beside the first.
  $d = Start-App 'untagged' -Untagged
  Check 'untagged check instance runs beside it' (Wait-Window $d)
  Close-App $d

  # 4. Closing the window frees the guard.
  Close-App $a
  $c = Start-App 'after-close'
  Check 'a start after the window closed opens a window' (Wait-Window $c)

  # 5. A killed instance never blocks the next start (Windows frees the mutex).
  Stop-Process -Id $c.Id -Force; [void]$c.WaitForExit(5000)
  $e = Start-App 'after-kill'
  Check 'a start after a kill opens a window' (Wait-Window $e)
  Close-App $e
} finally {
  foreach ($p in $script:mine) { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
}
"instance-check: $script:pass passed, $script:fail failed (logs in $work)"
if ($script:fail) { exit 1 }
