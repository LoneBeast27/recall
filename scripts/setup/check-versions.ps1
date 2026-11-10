<#
Release check for the shipped Recall executables in one folder (the release workflow runs it on
the payload, on Setup, and again after each signing round). A CI/developer tool: never shipped,
never started by Recall or Setup.

  -Directory  folder holding the .exe files to check (not recursive)
  -Version    major.minor.patch every exe must carry; empty = an unversioned build (only the
              product name is checked)
  -Count      how many .exe files the folder must hold
  -Signed     also require a valid, timestamped Authenticode signature from SignPath Foundation
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Directory,
    [AllowEmptyString()][string]$Version = '',
    [Parameter(Mandatory)][int]$Count,
    [switch]$Signed
)
$ErrorActionPreference = 'Stop'
if ($Version -and $Version -notmatch '^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$') {
    throw "Not a major.minor.patch version: '$Version'"
}
$files = @(Get-ChildItem -LiteralPath $Directory -Filter '*.exe' -File | Sort-Object Name)
if ($files.Count -ne $Count) { throw "Expected $Count .exe files in $Directory, found $($files.Count): $($files.Name -join ', ')" }

$signtool = $null
if ($Signed) {
    $signtool = Get-ChildItem -Path "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -File -ErrorAction SilentlyContinue |
        Sort-Object { [version]$_.Directory.Parent.Name } -Descending | Select-Object -First 1
}
foreach ($file in $files) {
    $info = $file.VersionInfo
    if ($info.ProductName -ne 'Recall') { throw "$($file.Name): ProductName is '$($info.ProductName)', not 'Recall'." }
    $numbers = '{0}.{1}.{2}.{3}' -f $info.FileMajorPart, $info.FileMinorPart, $info.FileBuildPart, $info.FilePrivatePart
    if ($Version -and $numbers -ne "$Version.0") { throw "$($file.Name): file version $numbers, not $Version.0." }
    $line = "$($file.Name): Recall $numbers"
    if ($Signed) {
        $signature = Get-AuthenticodeSignature -LiteralPath $file.FullName
        if ($signature.Status -ne 'Valid') { throw "$($file.Name): signature $($signature.Status) $($signature.StatusMessage)" }
        if (!$signature.TimeStamperCertificate) { throw "$($file.Name): the signature carries no timestamp." }
        if ($signature.SignerCertificate.Subject -notmatch 'SignPath Foundation') {
            throw "$($file.Name): unexpected signer $($signature.SignerCertificate.Subject)"
        }
        if ($signtool) {
            & $signtool.FullName verify /pa /tw /q $file.FullName
            if ($LASTEXITCODE -ne 0) { throw "$($file.Name): SignTool verification failed ($LASTEXITCODE)." }
        }
        $line += " | signed by $($signature.SignerCertificate.Subject)"
    }
    $line
}
