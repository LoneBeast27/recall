<#
  Report page check: in a private, offscreen Recall instance (its own settings, library, cache,
  control pipe and trace; no recorder; nothing reaches the real Recall data), clicks the
  sidebar's "Report a problem" row the way a person does and checks that the Report page opens,
  that Settings no longer carries the form, and that the page holds the form's controls.
  Prints one line per check and exits 1 if any fails.

    powershell -NoProfile -File scripts\dev\report-page-check.ps1 [-Exe <recall-native.exe>]
#>
param([string]$Exe = '')
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
. (Join-Path $PSScriptRoot 'viewer-check-lib.ps1')
Initialize-ViewerCheck $repo $Exe
$work = Join-Path $script:work 'report-page'
Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $work, (Join-Path $work 'clips') | Out-Null
$trace = Join-Path $work 'trace.jsonl'
$vars = @{
  RECALL_NATIVE_TRACE = $trace; RECALL_NATIVE_OFFSCREEN = '1'; RECALL_NATIVE_WINDOW_SIZE = '1280x1500'; RECALL_NO_CONTROLLER = '1'
  RECALL_CONTROL_PIPE = "recall-report-check-$PID"; RECALL_INSTANCE_TAG = "report-check-$PID"; RECALL_CPU_MASK = 'ffff0000'
  RECALL_OUT_DIR = (Join-Path $work 'clips'); RECALL_FOLDERS = (Join-Path $work 'folders.json'); RECALL_LIBRARY_META = (Join-Path $work 'library.json')
  RECALL_CACHE_DIR = (Join-Path $work 'cache'); APPDATA = $work; LOCALAPPDATA = $work
}
$saved = @{}
foreach ($k in $vars.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
try { $p = Start-Process -FilePath $script:exe -WorkingDirectory (Split-Path $script:exe) -NoNewWindow -PassThru; [void]$p.Handle }
finally { foreach ($k in $vars.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) } }
$t = Get-Date
while (-not $p.MainWindowHandle -or $p.MainWindowHandle -eq 0) {
  Start-Sleep -Milliseconds 50; $p.Refresh()
  if ($p.HasExited -or ((Get-Date) - $t).TotalSeconds -gt 20) { throw "no window (exited: $($p.HasExited))" }
}
$s = [pscustomobject]@{ Process = $p; Hwnd = $p.MainWindowHandle; Trace = $trace; Name = 'report-page' }
$fails = 0
function Check([string]$what, [bool]$ok) { if ($ok) { "PASS  $what" } else { "FAIL  $what"; $script:fails++ } }
try {
  [void](Wait-Trace $s { param($r) $r.route -eq 'library' -and $null -ne $r.targets.'nav-report' } 30000)
  Check 'the sidebar shows a Report a problem row' ($null -ne (Last $s).targets.'nav-report')
  Click $s 'nav-settings'
  $r = Wait-Trace $s { param($r) $r.route -eq 'settings' } 5000
  Check 'Settings opens' ($null -ne $r)
  $r = Settle $s
  Check 'Settings no longer holds the report form' ($null -eq $r.targets.'report-text')
  Click $s 'nav-report'
  $r = Wait-Trace $s { param($r) $r.route -eq 'report' -and $null -ne $r.targets.'report-text' } 5000
  Check 'the Report a problem row opens the Report page' ($null -ne $r)
  if ($r) {
    $names = $r.targets.PSObject.Properties.Name
    foreach ($control in 'report-text', 'report-add-media', 'report-save', 'report-clear') {
      Check "the Report page holds $control" ($names -contains $control)
    }
  }
} finally {
  Stop-Viewer $s
  Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}
if ($fails) { exit 1 }
