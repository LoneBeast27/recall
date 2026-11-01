<#
  One real export through the app on a real engine build (dev check; run after changes to the
  engine's command line, its caps stamp or the export path). Give it a RELEASE engine: a debug
  build keeps strings a release build does not.

    powershell -NoProfile -File scripts\dev\export-check.ps1 -Clip <mp4> -Exe <recall-native.exe> -Engine <recall.exe>
#>
param([Parameter(Mandatory)][string]$Clip, [Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$Engine)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
. (Join-Path $PSScriptRoot 'viewer-check-lib.ps1')
Initialize-ViewerCheck $repo $Exe
$script:only = @()
$env:RECALL_ENGINE = $Engine
Write-Host "engine: $Engine"
$stamped = Select-String -Path $Engine -Pattern 'recall-engine-caps:prepare-editor,export-edit;' -SimpleMatch -Quiet

Run-Scenario 'one real export on this engine' {
  $short = Join-Path $script:work 'export-source.mp4'
  if (-not (Test-Path $short)) { Invoke-Ffmpeg @('-t', '8', '-i', $Clip, '-map', '0', '-c', 'copy', $short) }
  $s = Start-Viewer 'export-real' $short
  $script:current = $s
  $folder = Join-Path (Split-Path $s.Clip) 'exports'
  Remove-Item (Join-Path $folder 'export-real-*') -ErrorAction SilentlyContinue
  $stems = Wait-Trace $s { param($r) $r.stems -and $r.stems[0] } 30000
  Click $s 'export'
  $t = Get-Date; $file = $null
  while (-not $file -and ((Get-Date) - $t).TotalSeconds -lt 120) {
    Start-Sleep -Milliseconds 250
    $file = Get-ChildItem $folder -Filter 'export-real-*.mp4' -ErrorAction SilentlyContinue | Where-Object { $_.Length -gt 0 } | Select-Object -First 1
  }
  $took = ((Get-Date) - $t).TotalSeconds
  Start-Sleep -Seconds 2
  $r = Last $s
  $length = if ($file) { [double](& ffprobe -v error -show_entries format=duration -of csv=p=0 $file.FullName) } else { 0 }
  Verdict @((Ok ($stamped -and $null -ne $stems) 'the engine carries the stamp and prepared the clip'), (Ok ($null -ne $file) "wrote $($file.Name), $(Fmt ($file.Length / 1MB)) MB after $(Fmt $took) s"), (Ok ($length -gt 7) "plays $(Fmt $length) s of 8"), (Ok (-not $r.status) "status '$($r.status)'"))
}

$fails = @($script:results | Where-Object { $_.Result -eq 'FAIL' }).Count
Write-Host "$($script:results.Count - $fails)/$($script:results.Count) passed"
exit $(if ($fails) { 1 } else { 0 })
