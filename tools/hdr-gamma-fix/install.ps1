<#
    install.ps1 — HDR SDR-gamma fix for ASUS PG27AQDP (AUS2740)

    Installs the gamma-2.2 MHC2 ICC profile into the Windows color store and
    associates it as the Advanced-Color (HDR) profile for the PG27AQDP, so that
    in HDR mode Windows decodes SDR-composited content with a pure gamma-2.2 EOTF
    instead of the sRGB piecewise curve (which lifts shadows / washes SDR).

    REVERSIBLE. Before touching anything it snapshots the current ICMProfileAC
    association for the target display; rollback.ps1 restores it exactly.

    Route: dantmnf/MHC2Gen  sdr-acm --calibrate-transfer --i-really-want-gamma-2.2
    Source: the machine's OWN newest Windows HDR Calibration profile
            (HDR Calibrated Profile 5-31-2026 85134.icc, 1300 nits).

    Run in a normal (non-elevated) PowerShell — the association is per-user (HKCU)
    and InstallColorProfileW writes to the shared store which is user-writable for
    the color spool dir. If store install fails with access denied, re-run elevated.

    Idempotent: safe to run twice; it will not duplicate the association.
#>

[CmdletBinding()]
param(
    [string]$ProfileName = 'PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm',
    # Target display device-instance key under ICM\ProfileAssociations. 0011 = PG27AQDP/AUS2740 on this machine.
    [string]$TargetInstance = '0011',
    [string]$TargetDisplayId = 'DISPLAY\AUS2740\5&14c483c&0&UID4357_0'
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$srcIcm = Join-Path $here $ProfileName
if (-not (Test-Path $srcIcm)) { throw "Profile not found next to script: $srcIcm" }

$colorDir = Join-Path $env:SystemRoot 'System32\spool\drivers\color'
$destIcm  = Join-Path $colorDir $ProfileName
$acmKey   = "HKCU:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ICM\ProfileAssociations\Display\{4d36e96e-e325-11ce-bfc1-08002be10318}\$TargetInstance"

Write-Host "== HDR gamma-2.2 fix installer ==" -ForegroundColor Cyan
Write-Host "Target display : $TargetDisplayId (instance $TargetInstance)"
Write-Host "Profile        : $ProfileName"

# --- 0. sanity: target association key must exist (proves the display slot is real) ---
if (-not (Test-Path $acmKey)) {
    throw "ACM association key not found: $acmKey`nThe PG27AQDP HDR profile slot does not exist. Re-run Windows HDR Calibration first, or pass the correct -TargetInstance."
}

# --- 1. snapshot current AC association for rollback (written next to script) ---
$before = (Get-ItemProperty -Path $acmKey -Name 'ICMProfileAC' -ErrorAction SilentlyContinue).ICMProfileAC
$snapshot = [ordered]@{
    CapturedUtc     = (Get-Date).ToUniversalTime().ToString('o')
    TargetInstance  = $TargetInstance
    TargetDisplayId = $TargetDisplayId
    AcmKey          = $acmKey
    ICMProfileAC    = @($before)     # array; last element = active default
    InstalledProfile = $ProfileName
}
$snapPath = Join-Path $here 'rollback-state.json'
$snapshot | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 $snapPath
Write-Host "[1] Rollback state saved -> $snapPath"
Write-Host "    prior ICMProfileAC = $($before -join '; ')"

# --- 2. install the profile into the color store ---
if (Test-Path $destIcm) {
    Write-Host "[2] Profile already present in store (overwriting file bytes to ensure latest)."
    Copy-Item -Path $srcIcm -Destination $destIcm -Force
} else {
    Add-Type -Namespace Mscms -Name Api -MemberDefinition @'
[DllImport("mscms.dll", CharSet=CharSet.Unicode, SetLastError=true)]
public static extern bool InstallColorProfileW(string pMachineName, string pProfileName);
'@
    $ok = [Mscms.Api]::InstallColorProfileW($null, $srcIcm)
    if (-not $ok) {
        $err = [System.Runtime.InteropServices.Marshal]::GetLastWin32Error()
        # Fallback: direct copy (store dir is usually user-writable)
        Write-Host "    InstallColorProfileW failed (err $err); falling back to file copy."
        Copy-Item -Path $srcIcm -Destination $destIcm -Force
    }
    Write-Host "[2] Profile installed to store -> $destIcm"
}

# --- 3. associate as the Advanced-Color (HDR) default for the target display ---
# ICMProfileAC is a REG_MULTI_SZ; the LAST entry is the active default that DWM/ACM uses.
# We remove any prior copy of our profile, then append it so it becomes the active default.
$list = @()
if ($before) { $list = @($before | Where-Object { $_ -and $_ -ne $ProfileName }) }
$newList = @($list + $ProfileName)
Set-ItemProperty -Path $acmKey -Name 'ICMProfileAC' -Value ([string[]]$newList) -Type MultiString
# make sure per-user profiles are honored (so HKCU association is the one used)
Set-ItemProperty -Path $acmKey -Name 'UsePerUserProfiles' -Value 1 -Type DWord
Write-Host "[3] Associated as Advanced-Color default. New ICMProfileAC = $($newList -join '; ')"

Write-Host ""
Write-Host "Done. The profile is installed and set as the active HDR/Advanced-Color profile." -ForegroundColor Green
Write-Host "It takes effect the next time the display enters HDR mode (toggle HDR OFF->ON, or"
Write-Host "sign out/in). SDR mode is unaffected (this profile only changes SDR-in-HDR decoding)."
Write-Host ""
Write-Host "Eyeball test: in HDR mode, a black-to-dark-grey gradient / dark UI should have"
Write-Host "deeper, non-milky shadows than before; mid-greys should look the same as SDR mode."
Write-Host "To revert: run rollback.ps1"
