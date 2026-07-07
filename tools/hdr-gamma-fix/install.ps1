<#
    install.ps1 — HDR SDR-gamma fix for ASUS PG27AQDP (AUS2740), two-variant

    Installs BOTH MHC2 gamma-2.2 ICC profiles into the Windows color store and
    associates the CHOSEN one as the active Advanced-Color (HDR) profile for the
    PG27AQDP. In HDR mode Windows then decodes SDR-composited content with a pure
    gamma-2.2 EOTF instead of the sRGB piecewise curve (which lifts shadows / washes
    SDR). Both variants keep the SAME pure gamma-2.2 transfer fix and an identity
    MHC2 matrix (HDR/PQ-native content is untouched). They differ ONLY in the gamut
    Windows ACM believes the panel has:

      accurate  (PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm)
                colorants = panel NATIVE wide gamut. ACM gamut-maps sRGB content to
                the native primaries -> colorimetrically correct sRGB (true, but
                reads as less saturated to someone used to the native stretch).

      punchy    (PG27AQDP_Punchy-Gamma2.2_MHC2.icm)   [DEFAULT]
                colorants = sRGB primaries. ACM sees "sRGB display" -> NO gamut
                compression -> sRGB code values land directly on the panel's native
                wide primaries = the familiar punchy native-gamut stretch.

    Switch variants with ONE command:
        .\install.ps1 -Variant punchy      # native-gamut stretch (active by default)
        .\install.ps1 -Variant accurate    # colorimetric sRGB
    Both profiles stay in the store; only the active association changes.

    REVERSIBLE. The FIRST run snapshots the true pre-fix ICMProfileAC association
    (the plain HDR Calibrated profiles, no MHC2) to rollback-state.json. Subsequent
    runs (including variant switches) NEVER overwrite a good pre-fix snapshot, and
    the installer refuses to let any MHC2 profile leak into the rollback target, so
    rollback.ps1 always restores the genuine pre-fix state.

    Run in a normal (non-elevated) PowerShell — the association is per-user (HKCU)
    and the color spool dir is user-writable. If store install fails with access
    denied, re-run elevated. Idempotent: safe to run repeatedly.
#>

[CmdletBinding()]
param(
    [ValidateSet('punchy','accurate')]
    [string]$Variant = 'punchy',
    [string]$AccurateProfile = 'PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm',
    [string]$PunchyProfile   = 'PG27AQDP_Punchy-Gamma2.2_MHC2.icm',
    # Target display device-instance key under ICM\ProfileAssociations. 0011 = PG27AQDP/AUS2740 on this machine.
    [string]$TargetInstance = '0011',
    [string]$TargetDisplayId = 'DISPLAY\AUS2740\5&14c483c&0&UID4357_0'
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

# both profiles this installer manages (so rollback / snapshotting can recognize them)
$managedProfiles = @($AccurateProfile, $PunchyProfile)
$activeProfile = if ($Variant -eq 'punchy') { $PunchyProfile } else { $AccurateProfile }

$colorDir = Join-Path $env:SystemRoot 'System32\spool\drivers\color'
$acmKey   = "HKCU:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ICM\ProfileAssociations\Display\{4d36e96e-e325-11ce-bfc1-08002be10318}\$TargetInstance"

Write-Host "== HDR gamma-2.2 fix installer (two-variant) ==" -ForegroundColor Cyan
Write-Host "Target display : $TargetDisplayId (instance $TargetInstance)"
Write-Host "Variant        : $Variant  ->  $activeProfile"

# --- 0. sanity: both profile files present next to script; target key exists ---
foreach ($p in $managedProfiles) {
    $src = Join-Path $here $p
    if (-not (Test-Path $src)) { throw "Profile not found next to script: $src" }
}
if (-not (Test-Path $acmKey)) {
    throw "ACM association key not found: $acmKey`nThe PG27AQDP HDR profile slot does not exist. Re-run Windows HDR Calibration first, or pass the correct -TargetInstance."
}

# --- 1. snapshot the TRUE pre-fix AC association (once) ---
# Only capture a snapshot if none exists yet, OR the existing one is invalid. Never
# overwrite a good pre-fix snapshot on a variant switch. Also strip any managed MHC2
# profile from a freshly-captured list so the rollback target can never contain one.
$before = (Get-ItemProperty -Path $acmKey -Name 'ICMProfileAC' -ErrorAction SilentlyContinue).ICMProfileAC
$snapPath = Join-Path $here 'rollback-state.json'

$haveGoodSnapshot = $false
if (Test-Path $snapPath) {
    try {
        $existing = Get-Content -Raw $snapPath | ConvertFrom-Json
        $existingList = @($existing.ICMProfileAC | Where-Object { $_ })
        $containsManaged = $false
        foreach ($e in $existingList) { if ($managedProfiles -contains $e) { $containsManaged = $true } }
        if ($existingList.Count -gt 0 -and -not $containsManaged) { $haveGoodSnapshot = $true }
    } catch { $haveGoodSnapshot = $false }
}

if ($haveGoodSnapshot) {
    Write-Host "[1] Existing pre-fix snapshot is valid; preserving it (no re-snapshot on variant switch)."
    Write-Host "    snapshot ICMProfileAC = $((Get-Content -Raw $snapPath | ConvertFrom-Json).ICMProfileAC -join '; ')"
} else {
    # Build a clean pre-fix list from the current registry value, dropping any managed MHC2 profiles.
    $preFix = @()
    if ($before) { $preFix = @($before | Where-Object { $_ -and ($managedProfiles -notcontains $_) }) }
    $snapshot = [ordered]@{
        CapturedUtc      = (Get-Date).ToUniversalTime().ToString('o')
        TargetInstance   = $TargetInstance
        TargetDisplayId  = $TargetDisplayId
        AcmKey           = $acmKey
        ICMProfileAC     = @($preFix)     # array; last element = active pre-fix default
        ManagedProfiles  = @($managedProfiles)
        InstalledProfile = $activeProfile
    }
    $snapshot | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 $snapPath
    Write-Host "[1] Rollback state saved -> $snapPath"
    Write-Host "    captured pre-fix ICMProfileAC = $($preFix -join '; ')"
}

# --- 2. install BOTH profiles into the color store ---
Add-Type -Namespace Mscms -Name Api -MemberDefinition @'
[DllImport("mscms.dll", CharSet=CharSet.Unicode, SetLastError=true)]
public static extern bool InstallColorProfileW(string pMachineName, string pProfileName);
'@ -ErrorAction SilentlyContinue

foreach ($p in $managedProfiles) {
    $src  = Join-Path $here $p
    $dest = Join-Path $colorDir $p
    if (Test-Path $dest) {
        Copy-Item -Path $src -Destination $dest -Force   # refresh bytes
        Write-Host "[2] Present in store (bytes refreshed): $p"
    } else {
        $ok = [Mscms.Api]::InstallColorProfileW($null, $src)
        if (-not $ok) {
            $err = [System.Runtime.InteropServices.Marshal]::GetLastWin32Error()
            Write-Host "    InstallColorProfileW failed for $p (err $err); falling back to file copy."
            Copy-Item -Path $src -Destination $dest -Force
        }
        Write-Host "[2] Installed to store: $p"
    }
}

# --- 3. associate the CHOSEN variant as the Advanced-Color (HDR) default ---
# ICMProfileAC is a REG_MULTI_SZ; the LAST entry is the active default that DWM/ACM uses.
# Keep any non-managed (real HDR Calibrated) profiles, plus the *other* managed variant
# present-but-not-last (so both remain available), then append the active one last.
$list = @()
if ($before) { $list = @($before | Where-Object { $_ }) }
# drop both managed profiles, then re-add: the inactive variant first, active variant last.
$list = @($list | Where-Object { $managedProfiles -notcontains $_ })
$inactiveProfile = if ($Variant -eq 'punchy') { $AccurateProfile } else { $PunchyProfile }
$newList = @($list + $inactiveProfile + $activeProfile)
Set-ItemProperty -Path $acmKey -Name 'ICMProfileAC' -Value ([string[]]$newList) -Type MultiString
Set-ItemProperty -Path $acmKey -Name 'UsePerUserProfiles' -Value 1 -Type DWord
Write-Host "[3] Associated '$Variant' as the active Advanced-Color default."
Write-Host "    New ICMProfileAC = $($newList -join '; ')"

Write-Host ""
Write-Host "Done. Both variants are in the store; '$Variant' is the active HDR/Advanced-Color profile." -ForegroundColor Green
Write-Host "Takes effect on the next HDR entry (toggle HDR OFF->ON, or sign out/in). SDR mode is unaffected."
Write-Host ""
if ($Variant -eq 'punchy') {
    Write-Host "Eyeball test (punchy): in HDR mode, saturated content (a bright red/green, game HUD,"
    Write-Host "vivid photo) should look PUNCHIER / more vivid than the accurate profile — matching the"
    Write-Host "native-wide-gamut look of SDR desktop mode. Shadows stay deep (gamma-2.2 fix intact)."
} else {
    Write-Host "Eyeball test (accurate): saturated content should look colorimetrically correct / true sRGB"
    Write-Host "(a touch less vivid than punchy). Shadows stay deep (gamma-2.2 fix intact)."
}
Write-Host ""
Write-Host "Switch:   .\install.ps1 -Variant punchy   |   .\install.ps1 -Variant accurate"
Write-Host "Revert:   .\rollback.ps1   (restores the true pre-fix state, removes both MHC2 profiles)"
