# Runs the pre-encode demo against the sandbox library only. Usage: run-demo.ps1 <max MB or none>
. (Join-Path $PSScriptRoot 'dev-paths.ps1')
$dev = Get-DevRoot (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent)   # RECALL_DEV_ROOT overrides
$b = Join-Path $dev '.tmp\recall-preencode-demo'
$app = Join-Path $env:LOCALAPPDATA 'Programs\Recall'
$env:PATH = "$app;$app\bin;$env:PATH"
$env:RECALL_FOLDERS = "$b\folders.json"
$env:RECALL_CACHE_DIR = "$b\cache"
$env:RECALL_ENGINE = Join-Path $dev '.tmp\recall-preencode\target\release\recall.exe'
if (Test-Path "$app\bin\ffmpeg.exe") { $env:RECALL_FFMPEG = "$app\bin\ffmpeg.exe"; $env:RECALL_FFPROBE = "$app\bin\ffprobe.exe" }
$clip = "$b\Raw\VALORANT__2026-11-02__16-23-32-315__dc0720f91d2c46c4937c9bf98d1ea405.mp4"
$demo = Join-Path $dev '.tmp\recall-preencode\target-gpui\release\examples\pre_encode_demo.exe'
if ($args[0] -and $args[0] -ne 'none') { & $demo $clip 97.946 119.936 $args[0] } else { & $demo $clip 97.946 119.936 }
