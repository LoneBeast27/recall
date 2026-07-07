# HDR SDR-Gamma Fix — ASUS PG27AQDP (QD-OLED, True Black 400)

Fixes the classic **"HDR mode makes SDR content look washed / milky shadows"** on
this machine, at the compositor level, reversibly. No DWM hooking / injection — a
plain ICC profile carrying **MHC2** tags that DWM + the GPU consume at scanout when
the display is in Advanced Color (HDR) mode.

## The bug

In HDR mode, Windows composites SDR content and decodes it with the **sRGB piecewise
EOTF**, while content and the panel actually assume **~pure gamma 2.2**. sRGB-piecewise
has a linear toe near black, so shadows get *lifted* → washed / grey-milky SDR.

The Windows HDR Calibration app's own profile does **not** fix this: its MHC2 tag
carries a gamut matrix + a **trivial identity regamma LUT** (2 entries `[0,1]`) and an
**sRGB-piecewise** SDR TRC. So the toe-lift is baked in.

## The fix (route chosen)

**dantmnf/MHC2Gen `sdr-acm --calibrate-transfer --i-really-want-gamma-2.2`**, run against
**this machine's own newest Windows HDR Calibration profile**
(`HDR Calibrated Profile 5-31-2026 85134.icc`, 1300 nits).

Why this route:
- **No native toggle exists.** Windows 11 build 26200 and NVIDIA 596.36 have *no*
  gamma-2.2-for-SDR-in-HDR desktop setting. NVIDIA "RTX HDR" is an AI SDR→HDR remap for
  *games*, not a compositor-level fix for desktop SDR content — different pipeline.
- **Machine-own profile > generic.** Generating from this panel's actual calibration
  (real primaries, white point, 1300-nit luminance, device gamma 2.199) is more accurate
  than dylanraga's generic `_400_` prebuilt. (dylanraga's prebuilt is the fallback if you
  ever need one — it's built on the same MHC2Gen.)
- **`sdr-acm` is the ACM/Advanced-Color mode**, which is exactly the SDR-in-HDR path.
  Its MHC2 **matrix is identity** and it does not touch the HDR/PQ path → HDR-native
  content is unaffected. Gamut mapping for SDR is done by Windows ACM from the profile's
  primaries/colorants, not by the MHC2 matrix.

What the generated profile carries (verified — see "Verification"):
- SDR TRC = **pure gamma 2.2** (ICC parametric type 0, g=2.2000) — replaces sRGB-piecewise
  (type 3). This is the load-bearing change ACM reads.
- MHC2 **identity** 3×4 matrix (no gamut CSC in the MHC2 matrix).
- MHC2 **regamma LUT** (1024 entries, monotonic 0→1) mapping gamma-2.2-encoded signal
  back through the panel's ~2.199 device EOTF.
- MaxCLL / Luminance = **1300 nits** (from the source calibration — sane).

### On the "overshot ~2610-nit" calibration concern

Checked. The ~2610-nit max-full-frame value is **not** written into any operative tag of
any of the three HDR Calibration profiles. All three carry sane `lumi` / MHC2 MaxCLL:
1170 (5-25-2025), 1000 (5-29-2026), **1300 (5-31-2026, the one we use)**. So the overshoot
did **not** poison the generated profile. (It would only matter in `hdr-decode` mode, where
max-nits scales the LUT; in `sdr-acm` the nits are metadata and SDR luminance is driven by
the Windows "SDR content brightness" slider, not MaxCLL.)

## Files in this folder

| File | What |
|---|---|
| `PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm` | The generated gamma-2.2 MHC2 profile (installed). |
| `install.ps1` | Snapshots current state → installs to store → associates as Advanced-Color default. |
| `rollback.ps1` | Restores the prior association exactly and removes the installed profile. |
| `rollback-state.json` | Machine-written snapshot used by rollback.ps1 (created at install). |
| `ROLLBACK_SNAPSHOT.txt` | Human-readable pre-change association dump (all display instances). |
| `bin/MHC2Gen.exe` | The generator (dantmnf CI build, self-contained AOT, lcms2 static). |
| `_src_newest.icc` | Copy of the source calibration profile the .icm was generated from (provenance). |

## What is installed (current state)

- Profile copied to `C:\Windows\System32\spool\drivers\color\PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm`.
- Associated on the PG27AQDP (`DISPLAY\AUS2740\...UID4357`, ICM instance `0011`) as the
  **last / active** entry of `ICMProfileAC` (the Advanced-Color/HDR profile slot), with
  `UsePerUserProfiles=1`. The SDR colorimetric slot (`ICMProfile`) was **not** touched, so
  the recall-hdr NvAPI SDR-mode gamut clamp is untouched.

Prior `ICMProfileAC` (restored by rollback):
`HDR Calibrated Profile 5-29-2026 53217.icc; HDR Calibrated Profile 5-31-2026 85134.icc`

## How to verify (machine-checkable)

```powershell
# profile in store
Test-Path "$env:SystemRoot\System32\spool\drivers\color\PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm"

# it is the active (last) Advanced-Color profile for the PG27AQDP
$k='HKCU:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ICM\ProfileAssociations\Display\{4d36e96e-e325-11ce-bfc1-08002be10318}\0011'
(Get-ItemProperty $k).ICMProfileAC   # last entry should be our .icm
```

MHC2 tag content was verified with a tag dump: TRC = gamma 2.2 (para type 0), identity
MHC2 matrix, 1024-entry monotonic regamma LUT, no negative colorants, MaxCLL 1300.

**Not machine-verifiable — awaits your eyes.** The transform applies at DWM/scanout, so it
does **not** appear in screen captures (same as the NvAPI SDR clamp). The final visual
verdict is yours.

## The one-line eyeball test

Toggle **HDR ON** (recall-hdr tray hotkey). Open a dark UI or a black→dark-grey gradient:
shadows should look **deeper / less milky** than before, while mid-greys match SDR mode.
If it looks *crushed* (too dark) instead, that's the opposite failure — run rollback.

> Takes effect on the next HDR entry: toggle HDR OFF→ON, or sign out/in.

## Rollback

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\rollback.ps1
```
Restores `ICMProfileAC` to the snapshot and removes the installed `.icm`. Then toggle
HDR OFF→ON. Add `-KeepProfileFile` to revert only the association and keep the file.

## Regenerate (if you re-run Windows HDR Calibration and want a fresh gamma-2.2 profile)

```powershell
# from a newer "HDR Calibrated Profile ....icc" in the color store:
.\bin\MHC2Gen.exe sdr-acm --calibrate-transfer --i-really-want-gamma-2.2 `
  --profile-desc "PG27AQDP SDR-in-HDR Gamma2.2 (MHC2)" `
  "<path to newest HDR Calibrated Profile .icc>" `
  ".\PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm"
# then re-run install.ps1
```
