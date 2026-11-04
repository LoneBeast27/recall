<#
  App-audio check: a real recording with three app tracks, read back from the saved clip.

  Copies the tone_player example to tone-a.exe and tone-b.exe ("apps"). Tone A (440 Hz) plays
  from the start, Tone B (660 Hz) starts -LateStart seconds into the engine run (the engine must
  find it while recording), and tone-c.exe is listed but never started (its track must be
  silence). The engine runs through engine-cost.ps1 (off-screen target, test mic only) with
  RECALL_AUDIO_APPS naming the three, then saves -SaveSeconds. The check reads the clip's tracks
  (names, order) and measures each tone in each track.

  Silence: the tones play ONLY on -Device, an output with no speakers behind it (a virtual
  device). tone_player refuses to start when no output matches and never uses the default.
  Nothing here holds a device name: pass it when you run the check.

    powershell -NoProfile -File scripts\dev\app-audio-check.ps1 -Device "<output name words>"
      [-TestMic "<mic name words>"] [-Engine <recall.exe>] [-Bin <dir of tone_player.exe>]
#>
param(
  [Parameter(Mandatory)] [string]$Device,
  [string]$TestMic = '',
  [string]$Engine = '',
  [string]$Bin = '',
  [string]$Target = '',
  [double]$LateStart = 12,
  [int]$SaveSeconds = 25,
  [string]$OutDir = ''
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
if (-not $Bin) { $Bin = Join-Path $repo 'target-dev\release\examples' }
if (-not $OutDir) { $OutDir = Join-Path $repo ".tmp\app-audio-check\$(Get-Date -Format yyyyMMdd-HHmmss)" }
$tone = Join-Path $Bin 'tone_player.exe'
if (-not (Test-Path $tone)) { throw "missing $tone (cargo build --release -p recall --example tone_player)" }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$apps = Join-Path $OutDir 'apps'; New-Item -ItemType Directory -Force -Path $apps | Out-Null
foreach ($n in 'a', 'b') { Copy-Item $tone (Join-Path $apps "tone-$n.exe") -Force }

function Start-Tone([string]$name, [int]$hz, [double]$seconds) {
  $psi = [Diagnostics.ProcessStartInfo]::new((Join-Path $apps $name), "--device `"$Device`" --hz $hz --seconds $seconds")
  $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
  $p = [Diagnostics.Process]::Start($psi)
  Start-Sleep -Milliseconds 800
  if ($p.HasExited) { throw "$name did not play: $($p.StandardError.ReadToEnd()) $($p.StandardOutput.ReadToEnd())" }
  $p
}

$total = 8 + 20 + 30
$a = Start-Tone 'tone-a.exe' 440 $total
$late = Start-Job -ScriptBlock {
  param($exe, $device, $wait, $seconds)
  Start-Sleep -Seconds $wait
  $psi = [Diagnostics.ProcessStartInfo]::new($exe, "--device `"$device`" --hz 660 --seconds $seconds")
  $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
  [void][Diagnostics.Process]::Start($psi)
} -ArgumentList (Join-Path $apps 'tone-b.exe'), $Device, $LateStart, $total

$env:RECALL_AUDIO_APPS = '[{"exe":"tone-a.exe","label":"Tone A"},{"exe":"tone-b.exe","label":"Tone B"},{"exe":"tone-c.exe","label":"Tone C"}]'
$costArgs = @('-NoProfile', '-File', (Join-Path $PSScriptRoot 'engine-cost.ps1'), '-Seconds', '20', '-SaveSeconds', "$SaveSeconds", '-OutDir', (Join-Path $OutDir 'clips'), '-Json', (Join-Path $OutDir 'cost.json'), '-MaxMsPerFrame', '100')
if ($Engine) { $costArgs += @('-Engine', $Engine) }
if ($Target) { $costArgs += @('-Target', $Target) }
if ($TestMic) { $costArgs += @('-TestMic', $TestMic) }
try {
  & powershell @costArgs | Tee-Object -FilePath (Join-Path $OutDir 'engine-cost.txt')
} finally {
  Remove-Item Env:RECALL_AUDIO_APPS -ErrorAction SilentlyContinue
  Wait-Job $late -Timeout 5 | Out-Null; Remove-Job $late -Force
  Get-Process -Name 'tone-a', 'tone-b' -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$apps*" } | Stop-Process -Force
}
$cost = Get-Content (Join-Path $OutDir 'cost.json') -Raw | ConvertFrom-Json
if (-not $cost.saved -or -not $cost.saved.path) { throw "no clip saved: $($cost.engine_log -join ' | ')" }
$clip = $cost.saved.path
"clip: $clip"
$cost.engine_log | Where-Object { $_ -match "audio track" }
& python.exe (Join-Path $PSScriptRoot 'app-audio-check.py') $clip
if ($LASTEXITCODE -ne 0) { "FAIL"; exit 1 }
"PASS"
