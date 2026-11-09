<#
.SYNOPSIS
Records separate Defender scans and SHA-256 hashes for a Recall release.
.EXAMPLE
.\scripts\dev\scan-release.ps1 -Setup .tmp\release\0.1.0\RecallSetup.exe -AppDirectory .tmp\release\0.1.0\recall-app -FfmpegArchive .tmp\ffmpeg.zip -FfmpegDirectory .tmp\ffmpeg\bin -VcRedist .tmp\vc_redist.x64.exe -EvidenceDirectory .tmp\release\0.1.0\scans
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Setup,
    [Parameter(Mandatory)][string]$AppDirectory,
    [Parameter(Mandatory)][string]$FfmpegArchive,
    [Parameter(Mandatory)][string]$FfmpegDirectory,
    [Parameter(Mandatory)][string]$VcRedist,
    [Parameter(Mandatory)][string]$EvidenceDirectory
)
$ErrorActionPreference = 'Stop'
$defender = Join-Path $env:ProgramData 'Microsoft\Windows Defender\Platform\CurrentVersion\MpCmdRun.exe'
if (!(Test-Path -LiteralPath $defender -PathType Leaf)) {
    $defender = Join-Path $env:ProgramFiles 'Windows Defender\MpCmdRun.exe'
}
$status = Get-MpComputerStatus
if (!$status.AntivirusEnabled -or !(Test-Path -LiteralPath $defender -PathType Leaf)) {
    throw 'Microsoft Defender Antivirus is unavailable.'
}
$output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($EvidenceDirectory)
if (Test-Path -LiteralPath $output) { throw "Choose an unused scan evidence directory: $output" }

$groups = [ordered]@{
    Setup = @($Setup)
    Recall = @('recall-native.exe','recall.exe','recall-hdr.exe','recall-overlay.exe' |
        ForEach-Object { Join-Path $AppDirectory $_ })
    'FFmpeg archive' = @($FfmpegArchive)
    'FFmpeg runtime' = @('ffmpeg.exe','ffprobe.exe','avcodec-61.dll','avdevice-61.dll','avfilter-10.dll',
        'avformat-61.dll','avutil-59.dll','postproc-58.dll','swresample-5.dll','swscale-8.dll' |
        ForEach-Object { Join-Path $FfmpegDirectory $_ })
    'Microsoft runtime' = @($VcRedist)
}

New-Item -ItemType Directory -Path $output | Out-Null
$status | Select-Object AMRunningMode,AntivirusEnabled,RealTimeProtectionEnabled,AMEngineVersion,AntivirusSignatureVersion |
    ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output 'defender.json')
$index = 0
$results = foreach ($group in $groups.GetEnumerator()) {
    foreach ($file in $group.Value) {
        $index++
        $item = Get-Item -LiteralPath $file
        if ($item.PSIsContainer) { throw "Expected a file: $file" }
        $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $item.FullName).Hash
        $log = '{0:00}-{1}.log' -f $index,$item.Name
        # Report detections for this scan without changing the test artifact or antivirus policy.
        & $defender -Scan -ScanType 3 -File $item.FullName -DisableRemediation *> (Join-Path $output $log)
        $code = $LASTEXITCODE
        [pscustomobject]@{
            Component = $group.Key
            File = $item.Name
            Bytes = $item.Length
            SHA256 = $hash
            DefenderExitCode = $code
            Log = $log
        }
    }
}
$results | Export-Csv -LiteralPath (Join-Path $output 'scan-results.csv') -NoTypeInformation
$results | Format-Table Component,File,Bytes,DefenderExitCode
if ($results.DefenderExitCode | Where-Object { $_ -ne 0 }) {
    throw 'Defender reported a detection or scan failure. Review the individual logs.'
}
