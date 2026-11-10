<#
  Writes this PC's machine-specific values to .local/machine.json (gitignored), so no tracked
  file carries a monitor id, a drive, a path or a user name. Detection is read-only: it lists
  the connected displays (WMI WmiMonitorID + the PnP monitor's driver key) and changes nothing.

    powershell -NoProfile -File scripts\dev\init-machine.ps1 -List
    powershell -NoProfile -File scripts\dev\init-machine.ps1 -HdrMonitor <name or id part> `
      -HdrProfileDir <folder> -HdrAccurateProfile <file.icm> -HdrPunchyProfile <file.icm> [-HdrStateDir <folder>]
    powershell -NoProfile -File scripts\dev\init-machine.ps1 -DevRoot <folder> -MainCheckout <folder>
    powershell -NoProfile -File scripts\dev\init-machine.ps1 -ClipLibrary <folder> -IslandBackdropClip <mp4> -Python <python.exe>

  Values given here are merged into the existing file; keys not given stay as they were.
  A value that is needed but not given is asked for when the session is interactive, else the
  script stops. -HdrStateDir defaults to %LOCALAPPDATA%\Recall\hdr-gamma-fix (a known folder).
  Template: .local.example\machine.example.json. Readers: scripts\dev\dev-paths.ps1.
#>
[CmdletBinding()]
param(
  [switch]$List,
  [string]$Out = '',
  [string]$HdrMonitor = '',
  [string]$HdrProfileDir = '',
  [string]$HdrAccurateProfile = '',
  [string]$HdrPunchyProfile = '',
  [string]$HdrStateDir = '',
  [string]$DevRoot = '',
  [string]$MainCheckout = '',
  [string]$ClipLibrary = '',          # paths.clipLibrary: a folder of real clips the viewer checks copy from
  [string]$IslandBackdropClip = '',   # clips.islandBackdrop: island-preview.ps1 -WebDir backdrop clip
  [string]$Python = ''                # tools.python: interpreter for island-preview.ps1
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
if (-not $Out) { $Out = Join-Path $repo '.local\machine.json' }

function Get-Displays {
  $pnp = @{}
  foreach ($d in @(Get-PnpDevice -Class Monitor -PresentOnly -ErrorAction SilentlyContinue)) {
    $drv = (Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_Driver' -ErrorAction SilentlyContinue).Data
    $pnp[$d.InstanceId.ToUpperInvariant()] = $drv
  }
  foreach ($m in @(Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorID -ErrorAction SilentlyContinue)) {
    $name = -join ($m.UserFriendlyName | Where-Object { $_ } | ForEach-Object { [char]$_ })
    $base = ($m.InstanceName -replace '_\d+$', '').ToUpperInvariant()
    $drv = $pnp[$base]
    [pscustomobject]@{
      Name       = $name
      DisplayId  = $m.InstanceName
      IcmKey     = if ($drv) { Split-Path $drv -Leaf } else { $null }
    }
  }
}

function Ask([string]$value, [string]$prompt) {
  if ($value) { return $value }
  if ([Environment]::UserInteractive -and -not ([Environment]::GetCommandLineArgs() -match '^-NonI')) {
    $v = Read-Host $prompt
    if ($v) { return $v }
  }
  throw "Missing value: $prompt (pass it as a parameter)."
}

function Set-Key($obj, [string]$name, $value) {
  if ($obj.PSObject.Properties.Name -contains $name) { $obj.$name = $value } else { $obj | Add-Member -NotePropertyName $name -NotePropertyValue $value }
}

$displays = @(Get-Displays)
if ($List) { $displays | Format-Table -AutoSize | Out-String | Write-Host; return }

$cfg = if (Test-Path $Out) { Get-Content -Raw $Out | ConvertFrom-Json } else { [pscustomobject]@{} }
$changed = $false

if ($HdrMonitor -or $HdrProfileDir -or $HdrAccurateProfile -or $HdrPunchyProfile -or $HdrStateDir) {
  $HdrMonitor = Ask $HdrMonitor 'Display for the HDR gamma fix (part of its name or id; see -List)'
  $hit = @($displays | Where-Object { $_.Name -like "*$HdrMonitor*" -or $_.DisplayId -like "*$HdrMonitor*" })
  if ($hit.Count -ne 1) {
    $displays | Format-Table -AutoSize | Out-String | Write-Host
    throw "'$HdrMonitor' matches $($hit.Count) displays; give a part that matches exactly one."
  }
  if (-not $hit[0].IcmKey) { throw "No driver key found for $($hit[0].DisplayId); cannot name its ICM association key." }
  if (-not $HdrStateDir) { $HdrStateDir = Join-Path $env:LOCALAPPDATA 'Recall\hdr-gamma-fix' }
  $hdr = if ($cfg.PSObject.Properties.Name -contains 'hdrGammaFix') { $cfg.hdrGammaFix } else { [pscustomobject]@{} }
  Set-Key $hdr 'targetDisplayId' $hit[0].DisplayId
  Set-Key $hdr 'targetInstance' $hit[0].IcmKey
  Set-Key $hdr 'profileDir' (Ask $HdrProfileDir 'Folder holding the generated .icm profiles')
  Set-Key $hdr 'accurateProfile' (Ask $HdrAccurateProfile 'File name of the accurate .icm')
  Set-Key $hdr 'punchyProfile' (Ask $HdrPunchyProfile 'File name of the punchy .icm')
  Set-Key $hdr 'stateDir' $HdrStateDir
  Set-Key $cfg 'hdrGammaFix' $hdr
  $changed = $true
}
if ($DevRoot -or $MainCheckout -or $ClipLibrary) {
  $paths = if ($cfg.PSObject.Properties.Name -contains 'paths') { $cfg.paths } else { [pscustomobject]@{} }
  if ($DevRoot) { Set-Key $paths 'devRoot' $DevRoot }
  if ($MainCheckout) { Set-Key $paths 'mainCheckout' $MainCheckout }
  if ($ClipLibrary) { Set-Key $paths 'clipLibrary' $ClipLibrary }
  Set-Key $cfg 'paths' $paths
  $changed = $true
}
if ($IslandBackdropClip) {
  $clips = if ($cfg.PSObject.Properties.Name -contains 'clips') { $cfg.clips } else { [pscustomobject]@{} }
  Set-Key $clips 'islandBackdrop' $IslandBackdropClip
  Set-Key $cfg 'clips' $clips
  $changed = $true
}
if ($Python) {
  $tools = if ($cfg.PSObject.Properties.Name -contains 'tools') { $cfg.tools } else { [pscustomobject]@{} }
  Set-Key $tools 'python' $Python
  Set-Key $cfg 'tools' $tools
  $changed = $true
}
if (-not $changed) { throw 'Nothing to write: pass -HdrMonitor ..., -DevRoot/-MainCheckout/-ClipLibrary, -IslandBackdropClip or -Python; or -List.' }

New-Item -ItemType Directory -Force -Path (Split-Path $Out) | Out-Null
$cfg | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 $Out
Write-Host "Wrote $Out"
Get-Content -Raw $Out | Write-Host
