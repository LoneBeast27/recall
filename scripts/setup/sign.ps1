[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$InputPath,
    [Parameter(Mandatory)][string]$OutputPath,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{40}$')][string]$CertificateThumbprint,
    [Parameter(Mandatory)][uri]$TimestampUrl
)
$ErrorActionPreference = 'Stop'
if (!$TimestampUrl.IsAbsoluteUri -or $TimestampUrl.Scheme -ne 'https') {
    throw 'Choose an HTTPS RFC 3161 timestamp service.'
}

$source = Get-Item -LiteralPath $InputPath
if ($source.PSIsContainer -or $source.Extension -ne '.exe') { throw 'Choose a Recall Setup executable.' }
$output = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputPath)
if ([IO.Path]::GetExtension($output) -ne '.exe' -or (Test-Path -LiteralPath $output)) {
    throw 'Choose a new .exe path for the signed copy.'
}

$certificate = Get-Item -LiteralPath "Cert:\CurrentUser\My\$CertificateThumbprint" -ErrorAction SilentlyContinue
$now = Get-Date
if (!$certificate -or !$certificate.HasPrivateKey -or $certificate.NotBefore -gt $now -or $certificate.NotAfter -le $now -or
    '1.3.6.1.5.5.7.3.3' -notin $certificate.EnhancedKeyUsageList.ObjectId.Value) {
    throw 'A valid code-signing certificate and key are required in the current user Personal certificate store.'
}

$sdk = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
$tool = Get-ChildItem -Path "$sdk\*\x64\signtool.exe" -File -ErrorAction SilentlyContinue |
    Sort-Object { [version]$_.Directory.Parent.Name } -Descending | Select-Object -First 1
if (!$tool) { throw 'Install the Windows SDK to use SignTool.' }

$parent = Split-Path -Parent $output
New-Item -ItemType Directory -Path $parent -Force | Out-Null
$staged = Join-Path $parent ("Recall-sign-" + [Guid]::NewGuid().ToString("N") + ".exe")
try {
    Copy-Item -LiteralPath $source.FullName -Destination $staged
    & $tool.FullName sign /fd SHA256 /sha1 $certificate.Thumbprint /tr $TimestampUrl.AbsoluteUri /td SHA256 $staged
    if ($LASTEXITCODE -ne 0) { throw "SignTool sign failed ($LASTEXITCODE)." }
    & $tool.FullName verify /pa /all /tw $staged
    if ($LASTEXITCODE -ne 0) { throw "SignTool verification failed ($LASTEXITCODE)." }
    $signature = Get-AuthenticodeSignature -LiteralPath $staged
    if ($signature.Status -ne 'Valid' -or !$signature.TimeStamperCertificate -or
        $signature.SignerCertificate.Thumbprint -ne $certificate.Thumbprint) {
        throw 'The installer did not verify with the selected publisher and timestamp.'
    }
    Move-Item -LiteralPath $staged -Destination $output
    Get-FileHash -LiteralPath $output
} finally {
    if (Test-Path -LiteralPath $staged) { Remove-Item -LiteralPath $staged -Force }
}
