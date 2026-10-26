<#
.SYNOPSIS
  Scripted on-screen check of the Recall Island (no controller needed).
.DESCRIPTION
  Starts recall-overlay with a --smoke-state fixture, captures the primary
  monitor's top-right corner at the given times, samples the process's CPU
  time and memory, and stacks the captures into one contact image.
  Without -Excluded the overlay runs with --smoke-allow-capture (capture
  exclusion off, so screen capture can see it); with -Excluded it keeps
  WDA_EXCLUDEFROMCAPTURE and the captures should show no island.
.EXAMPLE
  .\island-check.ps1 -Exe ..\.tmp\target-island\debug\recall-overlay.exe -Fixture fixtures\island-tour.json -OutDir ..\captures\recall\island -AtMs 150,1500,4050,5000,5750,7000,9500,10100,10800
#>
param(
    [Parameter(Mandatory)] [string]$Exe,
    [Parameter(Mandatory)] [string]$Fixture,
    [Parameter(Mandatory)] [string]$OutDir,
    [int[]]$AtMs = @(1500),
    [string]$Prefix = 'tour',
    [int]$Width = 760,
    [int]$Height = 110,
    [switch]$Excluded
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type -Namespace IslandCheck -Name Native -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(System.IntPtr value);'
[void][IslandCheck.Native]::SetProcessDpiAwarenessContext([IntPtr]-4)  # per-monitor v2: physical pixels
New-Item -ItemType Directory -Force $OutDir | Out-Null
$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$x = $screen.Right - $Width; $y = $screen.Top
$arguments = @('--smoke-state', "`"$((Resolve-Path $Fixture).Path)`"", '--diagnostics', "`"$OutDir\$Prefix-diagnostics.json`"")
if (-not $Excluded) { $arguments += '--smoke-allow-capture' }
$process = Start-Process -FilePath $Exe -ArgumentList $arguments -PassThru
$clock = [Diagnostics.Stopwatch]::StartNew()
$frames = @(); $peakMb = 0; $privateMb = 0; $samples = @()
foreach ($t in $AtMs) {
    while ($clock.ElapsedMilliseconds -lt $t) { Start-Sleep -Milliseconds 5 }
    $bitmap = New-Object System.Drawing.Bitmap $Width, $Height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($x, $y, 0, 0, $bitmap.Size)
    $graphics.Dispose()
    $file = Join-Path $OutDir "$Prefix-$t.png"
    $bitmap.Save($file); $bitmap.Dispose(); $frames += $file
    if (-not $process.HasExited) {
        $process.Refresh()
        $peakMb = [math]::Max($peakMb, $process.WorkingSet64 / 1MB)
        $privateMb = [math]::Max($privateMb, $process.PrivateMemorySize64 / 1MB)
        $samples += [pscustomobject]@{ at = $clock.ElapsedMilliseconds; cpu = $process.TotalProcessorTime.TotalMilliseconds }
    }
}
[void]$process.WaitForExit(30000)
# Steady state: CPU time between the first and last sample (startup excluded), as % of one core.
$steady = $null
if ($samples.Count -ge 2) {
    $first = $samples[0]; $last = $samples[-1]
    $steady = [math]::Round(100 * ($last.cpu - $first.cpu) / [math]::Max(1, $last.at - $first.at), 3)
}
# Contact sheet: every capture stacked, a 4 px gap between.
$sheet = New-Object System.Drawing.Bitmap $Width, (($Height + 4) * $frames.Count)
$graphics = [System.Drawing.Graphics]::FromImage($sheet)
$graphics.Clear([System.Drawing.Color]::Magenta)
for ($i = 0; $i -lt $frames.Count; $i++) {
    $image = [System.Drawing.Image]::FromFile($frames[$i]); $graphics.DrawImage($image, 0, $i * ($Height + 4)); $image.Dispose()
}
$graphics.Dispose(); $sheet.Save((Join-Path $OutDir "$Prefix-contact.png")); $sheet.Dispose()
[pscustomobject]@{
    screen = "$($screen.Width)x$($screen.Height)"
    exit_code = $process.ExitCode
    steady_cpu_percent_of_one_core = $steady
    steady_window_ms = if ($samples.Count -ge 2) { $samples[-1].at - $samples[0].at } else { 0 }
    startup_plus_run_cpu_ms = if ($samples.Count) { $samples[-1].cpu } else { $null }
    peak_working_set_mb = [math]::Round($peakMb, 1)
    peak_private_mb = [math]::Round($privateMb, 1)
    frames = $frames.Count
} | ConvertTo-Json
