# island-preview.ps1 - render the Recall Island previews offline (no window,
# no GPU: Direct2D software rendering) and save them as PNGs:
#   island-sheet.png        every island state on dark/light + appear/hide frames
#   notice-transform.png    an action notice's transformation, every 40 ms
#                           (over the stats pill, and with no stats showing)
#   notice-before-after.png the old content swap beside the transformation
#
#   powershell -NoProfile -File scripts/dev/island-preview.ps1 -Exe <recall-overlay.exe> -Out <dir> [-Scale 1.0]
#
# The filmstrips come with <image>.bmp.labels.tsv (x, y, px, text): row times
# and column titles, printed onto the PNG here.
#
# -WebDir <dir> instead (or as well) writes the web preview's inputs:
#   frames.png + manifest.json  every frame at 60 fps (--export-frames; straight
#                               RGBA, deflated by png-recompress.py, stdlib Python)
#   backdrop-valorant.jpg       one 2560x1440 frame of -Clip at -ClipAt s (ffmpeg, JPEG q85)
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$Out,
    [double]$Scale = 1.0,
    [string]$WebDir,
    [string]$Clip = 'V:\ALL RECORDINGS\VALORANT__2026-10-17__22-21-36-788__adaa986dc538484698bbabf313f2fa1e.mp4',
    [double]$ClipAt = 40,
    [string]$Python = 'C:\Program Files\Python311\python.exe'
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
if (-not $Out -and -not $WebDir) { throw 'give -Out (preview PNGs) and/or -WebDir (web preview frames)' }

function Export-Web([string]$Dir) {
    New-Item -ItemType Directory -Force $Dir | Out-Null
    & $Exe --export-frames $Dir
    if ($LASTEXITCODE -ne 0) { throw "--export-frames failed ($LASTEXITCODE)" }
    & $Python -I (Join-Path $PSScriptRoot 'png-recompress.py') (Join-Path $Dir 'frames.png')
    if ($LASTEXITCODE -ne 0) { throw "png-recompress failed ($LASTEXITCODE)" }
    $still = Join-Path $Dir 'backdrop.tmp.png'
    $at = $ClipAt.ToString([System.Globalization.CultureInfo]::InvariantCulture)
    & ffmpeg -v error -hwaccel none -threads 4 -ss $at -i $Clip -frames:v 1 -y $still
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed ($LASTEXITCODE)" }
    $img = [System.Drawing.Image]::FromFile($still)
    try {
        $jpeg = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq 'image/jpeg' }
        $params = New-Object System.Drawing.Imaging.EncoderParameters(1)
        $params.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter([System.Drawing.Imaging.Encoder]::Quality, [long]85)
        $img.Save((Join-Path $Dir 'backdrop-valorant.jpg'), $jpeg, $params)
    } finally { $img.Dispose() }
    Remove-Item $still
    Get-ChildItem $Dir | ForEach-Object { Write-Output ("{0}  {1} KB" -f $_.FullName, [math]::Round($_.Length / 1KB)) }
}

function Convert-Bmp([string]$In, [string]$Png) {
    $src = [System.Drawing.Image]::FromFile($In)
    $bmp = New-Object System.Drawing.Bitmap($src.Width, $src.Height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    try {
        $g.DrawImage($src, 0, 0, $src.Width, $src.Height)
        $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
        $brush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 200, 200, 200))
        $labels = "$In.labels.tsv"
        if (Test-Path $labels) {
            foreach ($line in Get-Content $labels) {
                $parts = $line -split "`t", 4
                if ($parts.Count -lt 4) { continue }
                $font = New-Object System.Drawing.Font('Segoe UI', [float]$parts[2], [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
                $g.DrawString($parts[3], $font, $brush, [float]$parts[0], [float]$parts[1])
                $font.Dispose()
            }
        }
        $bmp.Save($Png, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $g.Dispose(); $bmp.Dispose(); $src.Dispose()
    }
    Write-Output "$Png  $([math]::Round((Get-Item $Png).Length / 1KB)) KB"
}

if ($WebDir) { Export-Web $WebDir }
if (-not $Out) { return }
New-Item -ItemType Directory -Force $Out | Out-Null
$work = Join-Path ([System.IO.Path]::GetTempPath()) "island-preview-$PID"
New-Item -ItemType Directory -Force $work | Out-Null
try {
    $scaleArg = $Scale.ToString([System.Globalization.CultureInfo]::InvariantCulture)
    & $Exe --preview-sheet (Join-Path $work 'island-sheet.bmp') --preview-scale $scaleArg
    if ($LASTEXITCODE -ne 0) { throw "--preview-sheet failed ($LASTEXITCODE)" }
    & $Exe --preview-notice (Join-Path $work 'notice-transform.bmp') --preview-scale $scaleArg
    if ($LASTEXITCODE -ne 0) { throw "--preview-notice failed ($LASTEXITCODE)" }
    Convert-Bmp (Join-Path $work 'island-sheet.bmp') (Join-Path $Out 'island-sheet.png')
    Convert-Bmp (Join-Path $work 'notice-transform.bmp') (Join-Path $Out 'notice-transform.png')
    Convert-Bmp (Join-Path $work 'notice-transform-before-after.bmp') (Join-Path $Out 'notice-before-after.png')
} finally {
    Remove-Item -Recurse -Force $work
}
