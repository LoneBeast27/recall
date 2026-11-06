<#
  In-app update step (started by recall-native, services/update.rs, from a copy in
  %LOCALAPPDATA%\Recall\updates\<version>\). Runs hidden:
    1. waits for the app (-AppPid) to exit;
    2. checks the zip against SHA256SUMS.txt (a mismatch installs nothing);
    3. unpacks it and runs the release's own install.ps1 -Stage: the all-or-nothing swap
       (install-swap.ps1), which rolls back to this version on any failure;
    4. keeps Start with Windows as it was and restarts the recorder only if it was running;
    5. writes "ok <version>" or "failed <reason>" to updates\last.txt and reopens Recall.
#>
param([Parameter(Mandatory)][string]$Zip, [Parameter(Mandatory)][string]$Sums, [Parameter(Mandatory)][string]$InstallDir,
  [int]$AppPid = 0, [Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'
$updates = Join-Path $env:LOCALAPPDATA 'Recall\updates'
New-Item -ItemType Directory -Force -Path $updates | Out-Null
$log = Join-Path $updates 'update.log'
function Log([string]$m) { Add-Content -Path $log -Value ("{0:u} {1}" -f (Get-Date).ToUniversalTime(), $m) }
function Result([string]$line) { Set-Content -Path (Join-Path $updates 'last.txt') -Value $line -NoNewline }

Log "update to $Version from $Zip into $InstallDir"
try {
  if ($AppPid) {
    # The app saves edits and lets a running export finish before it exits.
    $p = Get-Process -Id $AppPid -ErrorAction SilentlyContinue
    if ($p -and -not $p.WaitForExit(600000)) { throw "Recall did not close within 10 minutes" }
  }
  $name = Split-Path $Zip -Leaf
  $want = (Get-Content $Sums | ForEach-Object { $_.Trim() } | Where-Object { $_ -match "^([0-9a-fA-F]{64})\s+\*?$([regex]::Escape($name))$" } | ForEach-Object { $Matches[1] } | Select-Object -First 1)
  if (-not $want) { throw "$name is not listed in SHA256SUMS.txt" }
  $have = (Get-FileHash -Algorithm SHA256 -LiteralPath $Zip).Hash
  if ($have -ne $want.ToUpperInvariant()) { throw "checksum mismatch for $name (download damaged); nothing was installed" }
  Log "checksum ok $have"

  $unpacked = Join-Path (Split-Path $Zip -Parent) 'unpacked'
  if (Test-Path $unpacked) { Remove-Item -LiteralPath $unpacked -Recurse -Force }
  Expand-Archive -LiteralPath $Zip -DestinationPath $unpacked
  $installer = Join-Path $unpacked 'install.ps1'
  if (-not (Test-Path $installer)) { throw "the release zip has no install.ps1" }

  $run = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'recall-hdr' -ErrorAction SilentlyContinue).'recall-hdr'
  $recorder = [bool](Get-Process -Name 'recall-hdr' -ErrorAction SilentlyContinue)
  $argv = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $installer, '-Stage', $unpacked, '-InstallDir', $InstallDir,
    '-NoShortcut', '-NoFrameTiming', '-CloseApp')
  if (-not $run) { $argv += '-NoAutostart' }
  if (-not $recorder) { $argv += '-NoStart' }
  $out = & powershell.exe @argv 2>&1
  $code = $LASTEXITCODE
  $out | ForEach-Object { Log "  $_" }
  if ($code -ne 0) { throw "installer exit $code (the previous version is unchanged)" }
  Result "ok $Version"
  Log "installed $Version"
} catch {
  Result "failed $($_.Exception.Message)"
  Log "FAILED $($_.Exception.Message)"
}
$app = Join-Path $InstallDir 'recall-native.exe'
if (Test-Path $app) { Start-Process -FilePath $app -WorkingDirectory $InstallDir }
