<#
    rollback.ps1 — revert the HDR SDR-gamma fix for PG27AQDP (AUS2740)

    Restores the ICMProfileAC (Advanced-Color/HDR) association for the target
    display to EXACTLY what it was before install.ps1 ran, using the snapshot in
    rollback-state.json. Then (optionally) removes the installed profile from the
    color store.

    This does NOT delete the Windows HDR Calibration profiles — it only undoes the
    association change and removes the profile file we added.

    Safe to run even if you're unsure — it is a no-op if no snapshot exists.
#>

[CmdletBinding()]
param(
    [switch]$KeepProfileFile   # if set, leaves the .icm in the color store (only reverts association)
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$snapPath = Join-Path $here 'rollback-state.json'

if (-not (Test-Path $snapPath)) {
    Write-Host "No rollback-state.json found next to this script — nothing to roll back." -ForegroundColor Yellow
    Write-Host "(If you installed manually, restore ICMProfileAC from ROLLBACK_SNAPSHOT.txt by hand.)"
    return
}

$snap = Get-Content -Raw $snapPath | ConvertFrom-Json
$acmKey = $snap.AcmKey
$profileName = $snap.InstalledProfile
Write-Host "== HDR gamma-2.2 fix rollback ==" -ForegroundColor Cyan
Write-Host "Target key : $acmKey"
Write-Host "Restoring  : ICMProfileAC = $($snap.ICMProfileAC -join '; ')"

if (-not (Test-Path $acmKey)) {
    Write-Host "Association key no longer exists ($acmKey). Display may have changed. Aborting association restore." -ForegroundColor Yellow
} else {
    $restore = @($snap.ICMProfileAC | Where-Object { $_ })   # drop nulls
    if ($restore.Count -gt 0) {
        Set-ItemProperty -Path $acmKey -Name 'ICMProfileAC' -Value ([string[]]$restore) -Type MultiString
        Write-Host "[1] ICMProfileAC restored to prior value."
    } else {
        # prior had no AC association at all -> remove the value
        Remove-ItemProperty -Path $acmKey -Name 'ICMProfileAC' -ErrorAction SilentlyContinue
        Write-Host "[1] Prior state had no ICMProfileAC; value removed."
    }
}

# remove the profile file we installed (unless asked to keep)
if (-not $KeepProfileFile) {
    $colorDir = Join-Path $env:SystemRoot 'System32\spool\drivers\color'
    $destIcm  = Join-Path $colorDir $profileName
    if (Test-Path $destIcm) {
        try {
            Add-Type -Namespace Mscms -Name Api2 -MemberDefinition @'
[DllImport("mscms.dll", CharSet=CharSet.Unicode, SetLastError=true)]
public static extern bool UninstallColorProfileW(string pMachineName, string pProfileName, bool bDelete);
'@
            $ok = [Mscms.Api2]::UninstallColorProfileW($null, $destIcm, $true)
            if (-not $ok) { Remove-Item -Path $destIcm -Force -ErrorAction SilentlyContinue }
            Write-Host "[2] Installed profile removed from store: $profileName"
        } catch {
            Remove-Item -Path $destIcm -Force -ErrorAction SilentlyContinue
            Write-Host "[2] Installed profile file deleted (fallback): $profileName"
        }
    } else {
        Write-Host "[2] Profile file not present in store; nothing to remove."
    }
} else {
    Write-Host "[2] -KeepProfileFile set; leaving profile in store."
}

Write-Host ""
Write-Host "Rollback complete. Toggle HDR OFF->ON (or sign out/in) for the display to pick up" -ForegroundColor Green
Write-Host "the restored association."
