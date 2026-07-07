# HDR SDR-Gamma Fix — ASUS PG27AQDP (QD-OLED, True Black 400)

Fixes the classic **"HDR mode makes SDR content look washed / milky shadows"** on
this machine, at the compositor level, reversibly. No DWM hooking / injection — a
plain ICC profile carrying **MHC2** tags that DWM + the GPU consume at scanout when
the display is in Advanced Color (HDR) mode.

## Which variant is active + how to switch (read first)

Two MHC2 profiles ship here. **Both carry the identical pure gamma-2.2 transfer fix
and an identity MHC2 matrix** (so HDR/PQ-native content is untouched). They differ
ONLY in the gamut Windows ACM believes the panel has:

| Variant | File | Colorants declared | ACM behavior | Look |
|---|---|---|---|---|
| **punchy** *(ACTIVE)* | `PG27AQDP_Punchy-Gamma2.2_MHC2.icm` | **sRGB** primaries | ACM sees an sRGB display → **no gamut compression** → sRGB code values land directly on the panel's native-wide primaries | native-wide-gamut **stretch** (matches the lifelong SDR-desktop punch) |
| accurate | `PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm` | panel **native wide** primaries | ACM gamut-maps sRGB content into the native gamut (compress) | colorimetrically **correct** sRGB (reads subtly less saturated) |

**Switch — one command each (per-user, reversible, takes effect on next HDR entry):**
```powershell
.\install.ps1 -Variant punchy      # native-gamut stretch  (currently ACTIVE — user's request)
.\install.ps1 -Variant accurate    # colorimetric true-sRGB
```
Switching only changes which profile is the *active/last* Advanced-Color association;
**both stay installed in the store** and both stay available. Variant switches never
touch the rollback snapshot — `rollback.ps1` always restores the true pre-fix state.

**How the punchy profile achieves no-gamut-compression (mechanism):** per the
Microsoft MHC2 pipeline, Windows reads the profile's colorant tags
(`rXYZ/gXYZ/bXYZ` + `wtpt`, exposed as ST.2086 primaries) to decide the display gamut,
and the MHC2 3×4 matrix is an *XYZ→XYZ adjustment only* (identity = pass-through). The
punchy profile is byte-identical to the accurate one **except** its colorant/`chrm`
tags are rewritten to **sRGB primaries** (0.64/0.33, 0.30/0.60, 0.15/0.06). Windows then
believes "source sRGB == display sRGB" and does **zero** colorimetric conversion, so the
raw sRGB code values drive the panel's native wide primaries at scanout — the familiar
saturation stretch. Matrix stays identity, TRC stays pure gamma-2.2, luminance stays
1300 nits. (This is the opposite of `MHC2Gen sdr-csc --source-gamut sRGB`, which bakes
an sRGB→native→sRGB *proofing/clamp* matrix — that would emulate sRGB, not stretch.)

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
| `PG27AQDP_Punchy-Gamma2.2_MHC2.icm` | **Punchy** variant (native-gamut stretch) — sRGB colorants. **ACTIVE.** |
| `PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm` | **Accurate** variant (true sRGB) — native-wide colorants. |
| `install.ps1` | `-Variant punchy\|accurate`. Snapshots pre-fix state (once) → installs BOTH to store → associates the chosen one as Advanced-Color default. |
| `rollback.ps1` | Restores the true pre-fix association and removes BOTH MHC2 profiles. `-KeepProfileFiles` keeps the files. |
| `rollback-state.json` | Machine-written pre-fix snapshot (holds `ManagedProfiles` = both variants). |
| `ROLLBACK_SNAPSHOT.txt` | Human-readable pre-change association dump (all display instances). |
| `bin/MHC2Gen.exe` | The generator (dantmnf CI build, self-contained AOT, lcms2 static). |
| `_src_newest.icc` | Copy of the source calibration profile the accurate .icm was generated from (provenance). |

## What is installed (current state)

- **Both** profiles copied to `C:\Windows\System32\spool\drivers\color\`.
- **Punchy is the ACTIVE** Advanced-Color profile on the PG27AQDP
  (`DISPLAY\AUS2740\...UID4357`, ICM instance `0011`) — it is the **last** entry of
  `ICMProfileAC` (the Advanced-Color/HDR slot), with `UsePerUserProfiles=1`. The accurate
  profile is present in the same list (available, not last). The SDR colorimetric slot
  (`ICMProfile`) was **not** touched, so the recall-hdr NvAPI SDR-mode gamut clamp is untouched.
- Current `ICMProfileAC`:
  `HDR Calibrated Profile 5-29-2026 53217.icc; HDR Calibrated Profile 5-31-2026 85134.icc; PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm; PG27AQDP_Punchy-Gamma2.2_MHC2.icm`

Pre-fix `ICMProfileAC` (restored by rollback):
`HDR Calibrated Profile 5-29-2026 53217.icc; HDR Calibrated Profile 5-31-2026 85134.icc`

> Note: `InstallColorProfileW` returns err 87 on this Windows build (an mscms P/Invoke
> quirk — it rejects both variants identically, even known-good ones), so the installer
> uses its file-copy fallback into the color store. DWM/ACM reads the legacy `ICMProfileAC`
> registry association, which is what makes the fix work — same path the original accurate
> profile used. (The modern `ColorProfileGetDisplayList` WCS API will show these profiles
> as unlisted because they were never WCS-DB-registered; that API is not the one Advanced
> Color consumes for these MHC2 profiles.)

## How to verify (machine-checkable)

```powershell
# both profiles in store
$c="$env:SystemRoot\System32\spool\drivers\color"
Test-Path "$c\PG27AQDP_Punchy-Gamma2.2_MHC2.icm"    # active variant
Test-Path "$c\PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm"   # accurate variant (still available)

# punchy is the active (last) Advanced-Color profile for the PG27AQDP
$k='HKCU:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ICM\ProfileAssociations\Display\{4d36e96e-e325-11ce-bfc1-08002be10318}\0011'
((Get-ItemProperty $k).ICMProfileAC)[-1]   # -> PG27AQDP_Punchy-Gamma2.2_MHC2.icm
```

MHC2 tag content verified (both variants) via tag dump: TRC = gamma 2.2 (para type 0),
**identity** MHC2 matrix, 1024-entry monotonic regamma LUT, no negative colorants,
MaxCLL 1300. Punchy differs only in colorant/`chrm` = sRGB primaries (accurate = native).

**Not machine-verifiable — awaits your eyes.** The transform applies at DWM/scanout, so it
does **not** appear in screen captures (same as the NvAPI SDR clamp). The final visual
verdict is yours.

## The one-line eyeball test

Toggle **HDR ON** (recall-hdr tray hotkey). Compare against SDR-desktop mode:

- **Punchy (active):** saturated content (a vivid red/green, a game HUD, a punchy photo)
  should look **as vivid / stretched as SDR-desktop mode** — the native-gamut punch you're
  used to — while shadows stay **deep / non-milky** (gamma-2.2 fix intact).
- If it instead looks *undersaturated / flat*, you're on the accurate profile — run
  `.\install.ps1 -Variant punchy`. If shadows look *crushed*, that's the opposite gamma
  failure — run `rollback.ps1`.

> Takes effect on the next HDR entry: toggle HDR OFF→ON, or sign out/in.

## Rollback

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\rollback.ps1
```
Restores `ICMProfileAC` to the true pre-fix snapshot and removes **both** MHC2 profiles.
Then toggle HDR OFF→ON. Add `-KeepProfileFiles` to revert only the association and keep
the files in the store.

## Regenerate (if you re-run Windows HDR Calibration)

```powershell
# ACCURATE (native-gamut colorants; ACM compresses sRGB -> true sRGB):
.\bin\MHC2Gen.exe sdr-acm --calibrate-transfer --i-really-want-gamma-2.2 `
  --profile-desc "PG27AQDP SDR-in-HDR Gamma2.2 (MHC2)" `
  "<path to newest HDR Calibrated Profile .icc>" `
  ".\PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm"
```

The **PUNCHY** variant is derived from the accurate one, not generated directly by
MHC2Gen (no MHC2Gen mode emits "sRGB colorants + identity matrix" — `sdr-acm` always
declares native colorants; `sdr-csc --source-gamut sRGB` bakes a proofing *clamp*
matrix). To regenerate punchy after regenerating accurate, re-run the derivation that
rewrites only the colorant/`chrm` tags to sRGB primaries and recomputes the profile ID
(`.tmp_src/` build script + the sRGB-in-PCS colorant values `rXYZ=(0.416980,0.214970,
0.015434) gXYZ=(0.360072,0.720727,0.103570) bXYZ=(0.151495,0.063401,0.773141)`, valid
for this profile's D65→D50 `chad`). Then `.\install.ps1 -Variant punchy`.
