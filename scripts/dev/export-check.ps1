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

Run-Scenario 'Cancel export stops this engine and leaves nothing' {
  $s = Start-Viewer 'export-stop' $Clip
  $script:current = $s
  $folder = Join-Path (Split-Path $s.Clip) 'exports'
  [void](Wait-Trace $s { param($r) $r.stems -and $r.stems[0] } 60000)
  Remove-Item (Join-Path $folder '*export-stop*') -Force -ErrorAction SilentlyContinue
  Click $s 'export'
  $t = Get-Date; $running = $null
  while (-not $running -and ((Get-Date) - $t).TotalSeconds -lt 10) { $running = Get-CimInstance Win32_Process -Filter "Name='recall.exe'" | Where-Object { $_.CommandLine -like '*--export-edit*' -and $_.CommandLine -like '*export-stop*' } | Select-Object -First 1 }
  $engine = if ($running) { Get-Process -Id $running.ProcessId -ErrorAction SilentlyContinue }
  if ($engine) { [void]$engine.Handle }
  Start-Sleep -Seconds 1
  Click $s 'export-cancel'
  $end = $null -ne $engine -and $engine.WaitForExit(5000)
  $said = Wait-Trace $s { param($r) -not $r.exporting } 5000
  Start-Sleep -Milliseconds 500
  $left = @(Get-ChildItem $folder -Force -ErrorAction SilentlyContinue | Where-Object { $_.Name -like '*export-stop*' })
  Verdict @((Ok ($null -ne $engine) "export running: pid $($engine.Id)"), (Ok $end 'the engine ended on Cancel'), (Ok ($null -ne $said) "export_error '$($said.export_error)'"), (Ok ($left.Count -eq 0) "$($left.Count) files left in exports"))
}

Run-Scenario 'Closing the window lets the export finish' {
  $s = Start-Viewer 'export-close' $Clip
  $script:current = $s
  $folder = Join-Path (Split-Path $s.Clip) 'exports'
  [void](Wait-Trace $s { param($r) $r.stems -and $r.stems[0] } 60000)
  Remove-Item (Join-Path $folder '*export-close*') -Force -ErrorAction SilentlyContinue
  Click $s 'export'
  $t = Get-Date; $running = $null
  while (-not $running -and ((Get-Date) - $t).TotalSeconds -lt 10) { $running = Get-CimInstance Win32_Process -Filter "Name='recall.exe'" | Where-Object { $_.CommandLine -like '*--export-edit*' -and $_.CommandLine -like '*export-close*' } | Select-Object -First 1 }
  $engine = if ($running) { Get-Process -Id $running.ProcessId -ErrorAction SilentlyContinue }
  if ($engine) { [void]$engine.Handle }
  Start-Sleep -Seconds 1
  [void][VC]::PostMessage($s.Hwnd, 0x10, [IntPtr]::Zero, [IntPtr]::Zero)  # WM_CLOSE
  $t = Get-Date
  while ([VC]::IsWindow($s.Hwnd) -and ((Get-Date) - $t).TotalSeconds -lt 5) { Start-Sleep -Milliseconds 50 }
  $closed = ((Get-Date) - $t).TotalSeconds
  $end = $null -ne $engine -and $engine.WaitForExit(120000)
  $gone = $s.Process.WaitForExit(10000)
  $left = @(Get-ChildItem $folder -Force -ErrorAction SilentlyContinue | Where-Object { $_.Name -like '*export-close*' })
  $file = $left | Where-Object { -not $_.Name.StartsWith('.') } | Select-Object -First 1
  $length = if ($file) { [double](& ffprobe -v error -show_entries format=duration -of csv=p=0 $file.FullName) } else { 0 }
  Verdict @((Ok ($null -ne $engine) "export running: pid $($engine.Id)"), (Ok ($closed -lt 5) "window gone after $(Fmt $closed) s"), (Ok $end 'the engine ended'), (Ok ($length -gt 0 -and $left.Count -eq 1) "$($left.Count) files in exports, $(Fmt $length) s long"), (Ok $gone 'the app ended after the export'))
}

Run-Scenario 'Closing the window writes the edit file' {
  $s = Start-Viewer 'export-edits' (Join-Path $script:work 'export-source.mp4')
  $script:current = $s
  $file = "$($s.Clip).recall-edit.json"
  Remove-Item $file -ErrorAction SilentlyContinue
  [void](Wait-Trace $s { param($r) $r.ready -and $r.clip } 30000)
  Key $s 'l' 1; [void](Settle $s 200); Key $s 'i'; [void](Settle $s 1000)
  $start = (Last $s).ranges[0][0]
  [void][VC]::PostMessage($s.Hwnd, 0x10, [IntPtr]::Zero, [IntPtr]::Zero)  # WM_CLOSE
  $gone = $s.Process.WaitForExit(5000)
  Verdict @((Ok ($start -gt 0) "trimmed to start at $(Fmt $start) s"), (Ok $gone 'the app ended'), (Ok (Test-Path $file) 'the edit file is beside the clip'))
}

$fails = @($script:results | Where-Object { $_.Result -eq 'FAIL' }).Count
Write-Host "$($script:results.Count - $fails)/$($script:results.Count) passed"
exit $(if ($fails) { 1 } else { 0 })
