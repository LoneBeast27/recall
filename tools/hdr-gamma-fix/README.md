# HDR SDR-Gamma Fix (MHC2 profile, per display)

Fixes the classic **"HDR mode makes SDR content look washed / milky shadows"** at the
compositor level, reversibly. No DWM hooking or injection: a plain ICC profile carrying
**MHC2** tags that DWM and the GPU consume at scanout when the display is in Advanced
Color (HDR) mode.

## The bug

In HDR mode Windows composites SDR content and decodes it with the **sRGB piecewise
EOTF**, while content and most panels assume **~pure gamma 2.2**. The sRGB curve has a
linear toe near black, so shadows get lifted: washed, grey-milky SDR. The profile from
the Windows HDR Calibration app does not fix this (its MHC2 tag carries an identity
regamma LUT and an sRGB-piecewise SDR TRC).

## The fix

[dantmnf/MHC2Gen](https://github.com/dantmnf/MHC2Gen)
`sdr-acm --calibrate-transfer --i-really-want-gamma-2.2`, run against **your own display's
newest Windows HDR Calibration profile**. The result carries a pure gamma-2.2 SDR TRC, an
identity MHC2 matrix (HDR/PQ content untouched) and a regamma LUT for the panel.

Two variants, identical except for the gamut Windows ACM believes the panel has:

| Variant | Colorants declared | Look |
|---|---|---|
| **punchy** (default) | sRGB primaries: ACM does no gamut compression | native-wide-gamut stretch |
| accurate | panel native primaries: ACM maps sRGB into the native gamut | colorimetrically correct sRGB |

`make-punchy.py <accurate.icm> <punchy.icm>` derives the punchy profile from the accurate
one (rewrites only the colorant/`chrm` tags and recomputes the profile ID).

## Machine-specific state lives outside the repo

The profiles are built from one display's calibration, and the rollback snapshot holds
that PC's registry state, so none of it is tracked. The scripts read this PC's values from
`.local/machine.json` (gitignored) under `hdrGammaFix`:

| Key | What |
|---|---|
| `profileDir` | folder holding the two generated `.icm` files |
| `accurateProfile`, `punchyProfile` | their file names |
| `stateDir` | where `install.ps1` writes `rollback-state.json` (suggested: `%LOCALAPPDATA%\Recall\hdr-gamma-fix`) |
| `targetInstance` | the display's key under `HKCU:\...\ICM\ProfileAssociations\Display\{4d36e96e-e325-11ce-bfc1-08002be10318}` |
| `targetDisplayId` | the display's PnP instance id (for the log and the snapshot) |

Create the file once per PC (read-only detection; it lists the displays it finds):

```powershell
powershell -NoProfile -File scripts\dev\init-machine.ps1 -HdrMonitor <part of the display's name or id> `
  -HdrProfileDir <folder with the .icm files> -HdrAccurateProfile <name.icm> -HdrPunchyProfile <name.icm>
```

Every value can also be given as a parameter (`-ProfileDir`, `-StateDir`, `-TargetInstance`,
...). A missing value stops the script with a message naming the key; nothing falls back to
a built-in default.

## Use

```powershell
.\install.ps1 -Variant punchy      # or -Variant accurate; switching keeps the first snapshot
.\rollback.ps1                     # restores the pre-fix association, removes both profiles
.\rollback.ps1 -KeepProfileFiles   # restores the association only
```

The first `install.ps1` run snapshots the pre-fix `ICMProfileAC` association into
`<stateDir>\rollback-state.json`; later runs never overwrite a good snapshot, and no MHC2
profile can leak into it. Changes take effect on the next HDR entry (toggle HDR off and on).
The transform applies at scanout, so it does not show in screen captures: judge it by eye.

## Regenerate (after re-running Windows HDR Calibration)

```powershell
.\bin\MHC2Gen.exe sdr-acm --calibrate-transfer --i-really-want-gamma-2.2 `
  --profile-desc "<display> SDR-in-HDR Gamma2.2 (MHC2)" `
  "<newest HDR Calibrated Profile .icc>" "<profileDir>\<accurate>.icm"
python make-punchy.py "<profileDir>\<accurate>.icm" "<profileDir>\<punchy>.icm"
```

`bin/` (the MHC2Gen CI build) is gitignored.
