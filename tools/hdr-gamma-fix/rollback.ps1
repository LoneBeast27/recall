<#
    rollback.ps1 — revert the HDR SDR-gamma fix for PG27AQDP (AUS2740)

    Restores the ICMProfileAC (Advanced-Color/HDR) association for the target
    display to EXACTLY the true pre-fix state captured in rollback-state.json
    (the plain HDR Calibrated profiles, no MHC2), then removes BOTH installed
    MHC2 variants (accurate + punchy) from the color store.

    This does NOT delete the Windows HDR Calibration profiles — it only undoes the
    association change and removes the MHC2 profile files this tool added.

    Safe to run even if you're unsure — it is a no-op if no snapshot exists.

    -KeepProfileFiles : revert only the association; leave both .icm files in store.
#>

[CmdletBinding()]
param(
    [switch]$KeepProfileFiles
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

# Profiles this tool installed and must remove. Prefer the snapshot's ManagedProfiles;
# fall back to the known names (and legacy single-InstalledProfile snapshots).
$managed = @()
if ($snap.PSObject.Properties.Name -contains 'ManagedProfiles' -and $snap.ManagedProfiles) {
    $managed = @($snap.ManagedProfiles | Where-Object { $_ })
}
if ($managed.Count -eq 0) {
    $managed = @('PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm','PG27AQDP_Punchy-Gamma2.2_MHC2.icm')
    if ($snap.InstalledProfile -and ($managed -notcontains $snap.InstalledProfile)) {
        $managed += $snap.InstalledProfile
    }
}

Write-Host "== HDR gamma-2.2 fix rollback ==" -ForegroundColor Cyan
Write-Host "Target key : $acmKey"
Write-Host "Restoring  : ICMProfileAC = $($snap.ICMProfileAC -join '; ')"
Write-Host "Removing   : $($managed -join ', ')"

if (-not (Test-Path $acmKey)) {
    Write-Host "Association key no longer exists ($acmKey). Display may have changed. Aborting association restore." -ForegroundColor Yellow
} else {
    # Restore exactly the pre-fix list, and defensively strip any managed profile that
    # might have crept into the snapshot so rollback can never re-assert an MHC2 profile.
    $restore = @($snap.ICMProfileAC | Where-Object { $_ -and ($managed -notcontains $_) })
    if ($restore.Count -gt 0) {
        Set-ItemProperty -Path $acmKey -Name 'ICMProfileAC' -Value ([string[]]$restore) -Type MultiString
        Write-Host "[1] ICMProfileAC restored to pre-fix value: $($restore -join '; ')"
    } else {
        # prior had no AC association at all -> remove the value
        Remove-ItemProperty -Path $acmKey -Name 'ICMProfileAC' -ErrorAction SilentlyContinue
        Write-Host "[1] Prior state had no ICMProfileAC; value removed."
    }
}

# remove the MHC2 profile files this tool installed (unless asked to keep)
if (-not $KeepProfileFiles) {
    $colorDir = Join-Path $env:SystemRoot 'System32\spool\drivers\color'
    Add-Type -Namespace Mscms -Name Api2 -MemberDefinition @'
[DllImport("mscms.dll", CharSet=CharSet.Unicode, SetLastError=true)]
public static extern bool UninstallColorProfileW(string pMachineName, string pProfileName, bool bDelete);
'@ -ErrorAction SilentlyContinue
    foreach ($p in $managed) {
        $dest = Join-Path $colorDir $p
        if (Test-Path $dest) {
            try {
                $ok = [Mscms.Api2]::UninstallColorProfileW($null, $dest, $true)
                if (-not $ok) { Remove-Item -Path $dest -Force -ErrorAction SilentlyContinue }
                Write-Host "[2] Removed from store: $p"
            } catch {
                Remove-Item -Path $dest -Force -ErrorAction SilentlyContinue
                Write-Host "[2] Deleted (fallback): $p"
            }
        } else {
            Write-Host "[2] Not present in store; nothing to remove: $p"
        }
    }
} else {
    Write-Host "[2] -KeepProfileFiles set; leaving both profiles in store."
}

Write-Host ""
Write-Host "Rollback complete. Toggle HDR OFF->ON (or sign out/in) for the display to pick up" -ForegroundColor Green
Write-Host "the restored pre-fix association."
