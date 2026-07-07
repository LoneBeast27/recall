# Masked HDR-flip experiment (2026-07-08)

**Question:** can the PG27AQDP's HDR-toggle transition (panel blackout + resync flash)
be hidden behind a deliberate full-screen black mask, so recall-hdr's automatic flips
feel seamless instead of jarring?

**RECORD CORRECTION — the continuous-rhythm runs yielded NO perceptual verdict.**
Mechanically the rig was proven: 6/6 real HDR transitions (3× SDR→HDR, 3× HDR→SDR)
fired 0.15–0.26s into their 1.5s black windows, log-verified. But the observer saw
**continuous black for the whole block** ("0 image in between") — the WPF overlay is
suspected to wedge solid-black across the first HDR mode change (device loss, never
re-presents) so the clear phases never showed. An imperceptibility claim from a run
where the observer could see nothing is a disengaged-probe reading — retracted.
Protocol v2 (one-shot masks, below) replaces the continuous rhythm.

## Protocol (final, after two corrupted attempts)

- Overlay: fullscreen topmost black, epoch-aligned rhythm **4.0s period / 1.5s black**,
  WPF with `WS_EX_NOACTIVATE|TRANSPARENT|TOOLWINDOW` + `ShowActivated=false`.
- Flip driver: global `Ctrl+Alt+H` (the manual-pin hotkey) sent 0.15–0.45s into a
  randomly scheduled black window; 6 flips over ~70s, ≥8s apart.
- Per-trial instrument gate: a trial counts only if `set_hdr_state` appears in
  recall-hdr.log within 4s (catches inert pins — see confounds).
- Rig: `Docker Speed Drive/.tmp/ab/{flicker,flipper,run-trials}.ps1`; evidence in
  `trials-log.txt` (00:53–00:55 IST run: 6/6 VALID, phases 161–254ms).

## Confounds burned into the rig (do not relearn)

1. **Foreground fullscreen = SdrGame session veto.** Any unknown fullscreen window
   (the mask overlay itself; fullscreen Brave video) commits an SdrGame session that
   makes the manual pin silently inert until that PROCESS EXITS (not until it leaves
   fullscreen). Fixes: overlay is non-activating (never foreground); user config got
   `[[overrides]] exe="brave" kind="none"`. Symptom in the log: repeated
   `manual HDR pin -> true` with no `set_hdr_state`.
2. **Leaked flipper daemon = double-fire.** Two daemons each send the hotkey →
   ON+OFF back-to-back → two stacked panel blackouts bleeding past the mask (the
   "black too long" incident). Fixes: run-trials pre-kills stale daemons; both
   daemons carry a max-lifetime auto-exit.
3. **Config quarantine.** An unparseable config.toml is renamed to `config.toml.bad`
   and the app runs pure defaults (`sdr_white` silently 360→220). Override kind
   tokens are `hdr_game | sdr_game | none` — there is no `non_game`.

## Protocol v2 — one-shot masks: TRANSITION MASKING VALIDATED (2026-07-08, ~01:10)

Continuous rhythm replaced by the product-shaped mechanic: a fresh WinForms
process per mask (fullscreen black 900ms, flip via hotkey at +150ms, internal
watchdog + driver timeout — a wedged window cannot outlive its trial). Runs:
6-blink block (4 real + 2 catch: 4/4 valid, 2/2 clean) + a blind 2-blink quiz,
observer confirmed full visibility between blinks. **The user could not detect
the transitions themselves** — no flash/artifact difference between real-flip
blinks and decoy blinks at 900ms hold.

**BUT the steady STATE gives it away: SDR-in-HDR is washed/greyed** ("greyed
out" on Discord dark theme = the classic sRGB-piecewise toe-lift). The MHC2
gamma fix is NOT being consumed on this machine despite a fully correct-looking
chain: `set_sdr_white` 360 asserted, punchy last in `ICMProfileAC` on legacy
instance `0011`, `UsePerUserProfiles=1`, both .icm in the store. Win11 26200 has
a **modern display-management instance key `MDM0`** holding only the stale
5-25-2025 calibration; mirroring the MHC2 profiles into MDM0's raw
`ICMProfileAC` + HDR re-entry did NOT fix it either (MDM0 mirror left in place).
Working hypothesis: 26200's stack only consumes profiles **registered in the WCS
database** (modern `ColorProfile*`/`WcsAssociateColorProfileWithDevice` path) —
the gamma-fix installer used the file-copy + raw-registry fallback because
`InstallColorProfileW` returns err 87, so the profiles were never DB-registered.

## Next build steps

1. **Fix profile consumption first** (blocks the punchy verdict AND makes flips
   truly seamless): register + associate the MHC2 profiles via the modern WCS
   API (mscms `WcsAssociateColorProfileWithDevice` / `ColorProfileAddDisplayAssociation`)
   against the PG27AQDP device key; verify by eye; then re-run the 2-blink quiz —
   success = the user cannot tell HDR state at all.
2. Then the one-shot mask inside recall-hdr's reconciler transitions: fade in
   ~150ms → EnableHdr/DisableHdr at +0.2s → hold ~0.9s → fade out. Driver-side
   flip measured at 0.3–0.9s.
