# Makes the Settings quality comparison crops (recall-native/assets/quality/*.png).
#
# A 360° panorama (equirectangular picture) is turned into a lossless 2 s clip of a fast camera
# turn: 2560x1440, 60 fps, a 90° wide view that turns -DegPerFrame degrees each frame (1.5° =
# 90°/s), re-projected every frame with ffmpeg's v360 filter. A real turn (perspective, sub-pixel
# motion that differs across the frame) is what a game sends; a flat pan of a still picture moves
# whole pixels and is far too easy for the encoder. That clip is recorded with h264_nvenc using
# the engine's settings (recall/src/encoder/settings.rs: preset, tune hq, VBR with peak = 2x
# target, no B-frames, 1 s GOP), once per Settings option. Frame 89 (half way through the second
# GOP, so neither a keyframe nor the first frames after one) is cropped at the same spot from
# every recording and from the lossless clip.
#
#   Bitrate:  15 / 25 / 40 / 60 Mbps at Balanced (p5).
#   Effort:   Performance (p4) and Quality (p7 + qres multipass + spatial AQ) at 15 Mbps.
#             Balanced at 15 Mbps is bitrate-15.png.
#
# Run it through the build gate (it uses NVENC): gated.ps1 none powershell -NoProfile -File ...
# Usage: make-previews.ps1 -Source <panorama> -CropX <px> -CropY <px> [-Size 136] [-Yaw 0] [-Pitch 0] [-DegPerFrame 1.5]
#   -Yaw/-Pitch aim the camera at frame 89 (degrees; yaw 0 = the picture's centre).
#   -Size is the crop in source px; the PNG is 2x that (pixel-doubled). -CropX/-CropY pick the crop inside the 2560x1440 frame (choose foliage or fine texture);
#   the whole frame 89 is written to <Work>\frame.png to choose them.
param(
    [Parameter(Mandatory)][string]$Source,
    [Parameter(Mandatory)][int]$CropX,
    [Parameter(Mandatory)][int]$CropY,
    [int]$Size = 136,
    [double]$Yaw = 0,
    [double]$Pitch = 0,
    [double]$DegPerFrame = 1.5,
    [string]$Out,
    [string]$Work = (Join-Path $env:TEMP 'recall-quality-previews'),
    [string]$Ffmpeg
)
$ErrorActionPreference = 'Stop'
# $PSScriptRoot is empty inside param() defaults under Windows PowerShell 5.1 when run via -File through the gate.
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $Out) { $Out = Join-Path $here '..\..\recall-native\assets\quality' }
if (-not $Ffmpeg) { $Ffmpeg = Join-Path $here '..\ffmpeg-dev\ffmpeg-7.1.1-gyan-full-shared\bin\ffmpeg.exe' }
New-Item -ItemType Directory -Force $Out, $Work | Out-Null
$Out = (Resolve-Path $Out).Path; $Source = (Resolve-Path $Source).Path
$frame = 89
$inv = [Globalization.CultureInfo]::InvariantCulture
function Run([string[]]$a) {
    & $Ffmpeg -hide_banner -loglevel error -y @a
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed: $($a -join ' ')" }
}
# ffmpeg logs to stderr; Windows PowerShell turns that into errors under 'Stop', so relax it here.
function Get-Metric([string[]]$a, [string]$pattern) {
    & { $ErrorActionPreference = 'Continue'; & $Ffmpeg -hide_banner @a 2>&1 } | ForEach-Object { "$_" } |
        Select-String $pattern | Select-Object -Last 1
}

# The camera yaw for every frame, sent to v360 by sendcmd (relative path: filter-graph escaping
# of a Windows path with a drive colon is fragile, so ffmpeg runs inside $Work).
$lines = foreach ($n in 0..119) {
    $t = ($n / 60).ToString('0.000000', $inv)
    $y = ($Yaw + ($n - $frame) * $DegPerFrame).ToString('0.0000', $inv)
    "$t v360 yaw $y;"
}
Set-Content -Path (Join-Path $Work 'yaw.cmd') -Value $lines -Encoding Ascii
$vfov = (2 * [Math]::Atan([Math]::Tan([Math]::PI / 4) * 1440 / 2560) * 180 / [Math]::PI).ToString('0.0000', $inv)
$p = $Pitch.ToString('0.0000', $inv)
# NUT keeps a 1/60 time base; MKV rounds to ms, and the SSIM filter then pairs the wrong frames.
$lossless = Join-Path $Work 'source.nut'
Push-Location $Work
try {
    Run @('-loop', '1', '-framerate', '60', '-i', $Source,
        '-vf', "sendcmd=f=yaw.cmd,v360=input=e:output=flat:h_fov=90:v_fov=${vfov}:pitch=${p}:w=2560:h=1440:interp=lanczos,format=yuv420p",
        '-frames:v', '120', '-c:v', 'ffv1', 'source.nut')
} finally { Pop-Location }

$variants = @(
    @{ Name = 'bitrate-15'; Preset = 'p5'; Mbps = 15; Max = $false },
    @{ Name = 'bitrate-25'; Preset = 'p5'; Mbps = 25; Max = $false },
    @{ Name = 'bitrate-40'; Preset = 'p5'; Mbps = 40; Max = $false },
    @{ Name = 'bitrate-60'; Preset = 'p5'; Mbps = 60; Max = $false },
    @{ Name = 'effort-performance'; Preset = 'p4'; Mbps = 15; Max = $false },
    @{ Name = 'effort-quality'; Preset = 'p7'; Mbps = 15; Max = $true }
)
Run @('-i', $lossless, '-vf', "select=eq(n\,$frame)", '-frames:v', '1', (Join-Path $Work 'frame.png'))
# Pixel-doubled here (nearest neighbour), so the app shows each crop 1:1 in its 272 px tile and
# the 2x zoom stays crisp instead of being smoothed by the GPU's linear upscale.
$crop = "select=eq(n\,$frame),crop=${Size}:${Size}:${CropX}:${CropY},scale=iw*2:ih*2:flags=neighbor"
# Setup also shows the matching full frame at an ordinary viewing scale.
$fullFrame = "select=eq(n\,$frame),scale=640:360:flags=lanczos"
Run @('-i', $lossless, '-vf', $fullFrame, '-frames:v', '1', (Join-Path $Out 'original-frame.png'))
$original = Join-Path $Out 'original.png'
Run @('-i', $lossless, '-vf', $crop, '-frames:v', '1', '-compression_level', '9', $original)
'{0,-20} {1,12} {2,14} {3,12}' -f 'variant', 'Mbps actual', 'clip SSIM', 'crop PSNR'
foreach ($v in $variants) {
    $clip = Join-Path $Work "$($v.Name).mp4"
    $png = Join-Path $Out "$($v.Name).png"
    $enc = @('-i', $lossless, '-c:v', 'h264_nvenc', '-preset', $v.Preset, '-tune', 'hq', '-rc', 'vbr',
        '-b:v', "$($v.Mbps)M", '-maxrate', "$($v.Mbps * 2)M", '-bf', '0', '-g', '60', '-r', '60')
    if ($v.Max) { $enc += @('-multipass', 'qres', '-spatial-aq', '1') }
    Run ($enc + @($clip))
    Run @('-i', $clip, '-vf', $crop, '-frames:v', '1', '-compression_level', '9', $png)
    if ($v.Name.StartsWith('bitrate-')) {
        Run @('-i', $clip, '-vf', $fullFrame, '-frames:v', '1', (Join-Path $Out "$($v.Name)-frame.png"))
    }
    # Whole-clip SSIM against the lossless clip, and PSNR of the shown crop against the original crop.
    $ssim = Get-Metric @('-i', $clip, '-i', $lossless, '-lavfi', '[0:v]setpts=N/60/TB[a];[1:v]setpts=N/60/TB[b];[a][b]ssim', '-f', 'null', '-') 'All:'
    $psnr = Get-Metric @('-i', $png, '-i', $original, '-lavfi', '[0:v][1:v]psnr', '-f', 'null', '-') 'average:'
    $bytes = (Get-Item $clip).Length
    '{0,-20} {1,12:N1} {2,14} {3,12}' -f $v.Name, ($bytes * 8 / 2 / 1e6),
        ($ssim -replace '.*All:(\S+).*', '$1'), (($psnr -replace '.*average:(\S+).*', '$1') + ' dB')
}
Get-ChildItem $Out -Filter *.png | Select-Object Name, Length | Format-Table -AutoSize
'total PNG bytes: {0}' -f (Get-ChildItem $Out -Filter *.png | Measure-Object Length -Sum).Sum
