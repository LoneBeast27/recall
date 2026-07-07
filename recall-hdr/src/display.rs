//! CCD (Connecting and Configuring Displays) wrapper: target resolution by
//! friendly name, HDR get/set with the build-26200 API-variant probe, and the
//! SDR white level.
//!
//! # Why CCD (not NvAPI/DXGI) for HDR + white level
//! The gamut clamp ([`crate::clamp`]) is NVIDIA-specific; HDR + SDR-white are
//! *adapter-agnostic* and this box is a hybrid GPU (RTX 5090 + AMD iGPU). CCD
//! (`QueryDisplayConfig` + `DisplayConfigGetDeviceInfo`) addresses a display by
//! its **(adapter LUID, target id)** pair regardless of which GPU drives it, and
//! matches a *specific* panel by `monitorFriendlyDeviceName` -- critical: we
//! manage the ASUS "PG27AQDP" and must NEVER touch the BenQ.
//!
//! # HDR API variants on Windows 11 build 26200 (24H2) -- verified intel
//! 24H2 split "advanced color" (WCG) from "HDR". Two generations; we try NEW
//! first, fall back to LEGACY on any error:
//! - **NEW (24H2+, hand-rolled -- not in `windows 0.62` metadata):**
//!   `GET_ADVANCED_COLOR_INFO_2` (type 15) exposes an explicit
//!   `highDynamicRangeUserEnabled` bit; `SET_HDR_STATE` (type 16,
//!   `{header, u32 enableHdr:1}`) toggles HDR *only*. Values from 24H2 SDK
//!   `SDK_26100.h`, via xbmc PR #26096.
//! - **LEGACY (in-metadata):** `GET_ADVANCED_COLOR_INFO` (9) +
//!   `SET_ADVANCED_COLOR_STATE` (10, `enableAdvancedColor`). Conflates HDR + WCG
//!   on a wide-gamut panel, but it is the only pre-24H2 path.
//!
//! We **always GET before SET** and no-op (return `Ok`) when already in the
//! desired state -- avoids a flash + honors reconciler idempotency.
//!
//! # SDR white level (nits) -- unit convention
//! Public `DISPLAYCONFIG_SDR_WHITE_LEVEL.SDRWhiteLevel` is fixed-point where
//! **1000 == 80 nits** (nits = level*80/1000); `get_sdr_white` returns *nits*.
//! SET is UNDOCUMENTED device-info type `0xFFFFFFEE` (reverse-engineered by
//! ledoge, whose novideo_srgb CSC `clamp.rs` also ports): struct
//! `{header, u32 SDRWhiteLevel, u8 finalValue=1}`, inverse nits->level =
//! nits*1000/80. Panel note: PG27AQDP QD-OLED has aggressive ABL, so the sane
//! SDR-white default while HDR is on is ~200-250 nits (level 2500..=3125).

use std::os::windows::ffi::OsStringExt;

use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes,
    QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
    DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, LUID};

use crate::log;

// ---------------------------------------------------------------------------
// Public identity type
// ---------------------------------------------------------------------------

/// A resolved display target: the CCD address (`adapter LUID` + `target id`) plus
/// a cached friendly name for logging. `Clone` so callers can stash it in config.
///
/// The LUID is not `Serialize`-stable across reboots (Windows may re-issue it), so
/// persisted config should re-resolve by `name` on startup rather than trust a
/// stored `TargetId`. We keep the raw parts here only for the live session.
#[derive(Debug, Clone)]
pub struct TargetId {
    adapter: LUID,
    id: u32,
    name: String,
}

impl TargetId {
    /// The friendly monitor name this target resolved from (e.g. "PG27AQDP").
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl PartialEq for TargetId {
    /// Two targets are the same panel iff the CCD address matches. The cached
    /// name is descriptive only and excluded from identity.
    fn eq(&self, other: &Self) -> bool {
        self.adapter.LowPart == other.adapter.LowPart
            && self.adapter.HighPart == other.adapter.HighPart
            && self.id == other.id
    }
}
impl Eq for TargetId {}

// ---------------------------------------------------------------------------
// Hand-rolled 24H2 constants (not in windows 0.62 metadata)
// ---------------------------------------------------------------------------

// Type values + sources documented in the module header. GET_ACI2/SET_HDR_STATE
// from the 24H2 SDK (xbmc PR #26096); SET white 0xFFFFFFEE from ledoge/set_maxtml
// (the `i32` cast preserves the bit pattern: `-18i32 as u32 == 0xFFFFFFEE`).
const GET_ADVANCED_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE =
    DISPLAYCONFIG_DEVICE_INFO_TYPE(15);
const SET_HDR_STATE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(16);
const SET_SDR_WHITE_LEVEL: DISPLAYCONFIG_DEVICE_INFO_TYPE =
    DISPLAYCONFIG_DEVICE_INFO_TYPE(0xFFFF_FFEEu32 as i32);

/// SDR white level fixed-point scale: `SDRWhiteLevel` units per 80 nits = 1000.
const SDR_WHITE_UNITS_PER_80_NITS: u32 = 1000;

/// Hand-rolled `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2` (24H2). `#[repr(C)]` MUST
/// match the SDK header: `{header; union{bitfield; u32 value}; u32 colorEncoding;
/// u32 bitsPerColorChannel; u32 activeColorMode}` -- union modeled as bare `value`
/// (same size); we read HDR bits from `value` by mask.
#[repr(C)]
#[derive(Clone, Copy)]
struct GetAdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode: i32,
}

/// Bit 4 of `GetAdvancedColorInfo2.value`: `highDynamicRangeSupported`.
const ACI2_HDR_SUPPORTED: u32 = 1 << 4;
/// Bit 5: `highDynamicRangeUserEnabled` -- the "is HDR currently on" bit we want.
const ACI2_HDR_USER_ENABLED: u32 = 1 << 5;

/// Hand-rolled `DISPLAYCONFIG_SET_HDR_STATE` (24H2): `{header; u32 value}` where
/// bit0 of `value` = `enableHdr`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SetHdrState {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
}

/// Hand-rolled undocumented `DISPLAYCONFIG_SET_SDR_WHITE_LEVEL` (ledoge):
/// `{header; u32 SDRWhiteLevel; u8 finalValue}`; `finalValue` must be 1.
#[repr(C)]
#[derive(Clone, Copy)]
struct SetSdrWhiteLevel {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    sdr_white_level: u32,
    final_value: u8,
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Decode a NUL-terminated wide buffer into a String (mirrors edid.rs).
fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    std::ffi::OsString::from_wide(&buf[..end])
        .to_string_lossy()
        .into_owned()
}

/// A fresh header for a get/set packet of type `t`, sized for struct `S`,
/// addressed to `(adapter, id)`.
fn header_for<S>(
    t: DISPLAYCONFIG_DEVICE_INFO_TYPE,
    adapter: LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: t,
        size: std::mem::size_of::<S>() as u32,
        adapterId: adapter,
        id,
    }
}

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

/// Query all *active* display paths and return their raw `(adapter, target id)`
/// pairs. Skips inactive/unavailable targets so a deprecated virtual adapter
/// (e.g. a leftover Parsec target) never shows up.
fn active_targets() -> Result<Vec<(LUID, u32)>, String> {
    unsafe {
        let mut n_paths: u32 = 0;
        let mut n_modes: u32 = 0;
        let st = GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes);
        if st != ERROR_SUCCESS {
            return Err(format!("GetDisplayConfigBufferSizes failed: {st:?}"));
        }

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        let st = QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_paths,
            paths.as_mut_ptr(),
            &mut n_modes,
            modes.as_mut_ptr(),
            None,
        );
        if st != ERROR_SUCCESS {
            return Err(format!("QueryDisplayConfig failed: {st:?}"));
        }
        paths.truncate(n_paths as usize);

        // Skip anything the OS marks unavailable (drops leftover virtual targets).
        Ok(paths
            .iter()
            .filter(|p| p.targetInfo.targetAvailable.as_bool())
            .map(|p| (p.targetInfo.adapterId, p.targetInfo.id))
            .collect())
    }
}

/// GET the friendly name for a raw target, or `None` if the query fails or the
/// name is empty (some virtual targets report a blank friendly name).
fn friendly_name(adapter: LUID, id: u32) -> Option<String> {
    unsafe {
        let mut req = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
        req.header = header_for::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(
            DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            adapter,
            id,
        );
        if DisplayConfigGetDeviceInfo(&mut req.header) != ERROR_SUCCESS.0 as i32 {
            return None;
        }
        let name = wide_to_string(&req.monitorFriendlyDeviceName);
        (!name.is_empty()).then_some(name)
    }
}

/// Enumerate all active targets with a friendly name, for logging / a config UI.
pub fn enumerate_targets() -> Result<Vec<(String, TargetId)>, String> {
    let mut out = Vec::new();
    for (adapter, id) in active_targets()? {
        if let Some(name) = friendly_name(adapter, id) {
            out.push((name.clone(), TargetId { adapter, id, name }));
        }
    }
    Ok(out)
}

/// Resolve a display by its `monitorFriendlyDeviceName` (e.g. "PG27AQDP").
/// Case-insensitive exact match. Errors if not found among active targets.
pub fn resolve_target(friendly_name_wanted: &str) -> Result<TargetId, String> {
    enumerate_targets()?
        .into_iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(friendly_name_wanted))
        .map(|(_, t)| t)
        .ok_or_else(|| {
            format!("no active display named '{friendly_name_wanted}' (connected + on?)")
        })
}

// ---------------------------------------------------------------------------
// HDR get/set
// ---------------------------------------------------------------------------

/// GET the current HDR-enabled state via the NEW (24H2) API. `Ok(Some(bool))` on
/// success, `Ok(None)` if the driver rejects the newer type (caller falls back).
fn get_hdr_new(t: &TargetId) -> Result<Option<bool>, String> {
    unsafe {
        // Zero the whole struct (matches windows-rs's `mem::zeroed` defaults), then
        // stamp the header. Driver fills `value` on success.
        let mut req: GetAdvancedColorInfo2 = std::mem::zeroed();
        req.header =
            header_for::<GetAdvancedColorInfo2>(GET_ADVANCED_COLOR_INFO_2, t.adapter, t.id);
        if DisplayConfigGetDeviceInfo(&mut req.header as *mut _) != ERROR_SUCCESS.0 as i32 {
            return Ok(None);
        }
        // Not HDR-capable at all -> definitively "off".
        if req.value & ACI2_HDR_SUPPORTED == 0 {
            return Ok(Some(false));
        }
        Ok(Some(req.value & ACI2_HDR_USER_ENABLED != 0))
    }
}

/// GET HDR state via the LEGACY (pre-24H2) `GET_ADVANCED_COLOR_INFO` (type 9).
/// `_bitfield` bit1 = `advancedColorEnabled` (conflates HDR + WCG on a wide panel,
/// but the only signal on older builds).
fn get_hdr_legacy(t: &TargetId) -> Result<bool, String> {
    unsafe {
        let mut req = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::default();
        req.header = header_for::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            t.adapter,
            t.id,
        );
        let st = DisplayConfigGetDeviceInfo(&mut req.header);
        if st != ERROR_SUCCESS.0 as i32 {
            return Err(format!("GET_ADVANCED_COLOR_INFO failed: status {st}"));
        }
        Ok(req.Anonymous.Anonymous._bitfield & 0b10 != 0)
    }
}

/// GET whether Windows HDR is currently ON. Tries the NEW 24H2 API first (accurate
/// HDR-only bit), falls back to LEGACY advanced-color.
pub fn get_hdr_state(t: &TargetId) -> Result<bool, String> {
    match get_hdr_new(t)? {
        Some(on) => Ok(on),
        None => get_hdr_legacy(t),
    }
}

/// SET HDR via NEW (24H2) `SET_HDR_STATE` (type 16). `Ok(true)` on success,
/// `Ok(false)` if the driver rejected the newer type (caller falls back).
fn set_hdr_new(t: &TargetId, on: bool) -> Result<bool, String> {
    unsafe {
        let mut req = SetHdrState {
            header: header_for::<SetHdrState>(SET_HDR_STATE, t.adapter, t.id),
            value: on as u32,
        };
        let st = DisplayConfigSetDeviceInfo(&mut req.header as *const _);
        Ok(st == ERROR_SUCCESS.0 as i32)
    }
}

/// SET HDR via LEGACY `SET_ADVANCED_COLOR_STATE` (type 10, bit0 = `enableAdvancedColor`).
fn set_hdr_legacy(t: &TargetId, on: bool) -> Result<(), String> {
    unsafe {
        let mut req = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE::default();
        req.header = header_for::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>(
            DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
            t.adapter,
            t.id,
        );
        req.Anonymous.Anonymous._bitfield = on as u32;
        let st = DisplayConfigSetDeviceInfo(&req.header);
        if st != ERROR_SUCCESS.0 as i32 {
            return Err(format!("SET_ADVANCED_COLOR_STATE failed: status {st}"));
        }
        Ok(())
    }
}

/// Set Windows HDR on/off. **GET-before-SET**: no-op (returns `Ok`, no SET API
/// call) if already in the desired state -- avoids a flash + honors idempotency.
/// Prefers the HDR-only 24H2 path, falls back to legacy advanced-color.
pub fn set_hdr_state(t: &TargetId, on: bool) -> Result<(), String> {
    let current = get_hdr_state(t)?;
    if current == on {
        log!("set_hdr_state({}, {on}) -- already {on}, no-op", t.name);
        return Ok(());
    }
    log!("set_hdr_state({}, {on}) -- flipping from {current}", t.name);
    if set_hdr_new(t, on)? {
        return Ok(());
    }
    log!("SET_HDR_STATE (new) rejected; falling back to legacy SET_ADVANCED_COLOR_STATE");
    set_hdr_legacy(t, on)
}

// ---------------------------------------------------------------------------
// SDR white level
// ---------------------------------------------------------------------------

/// GET the SDR white level for this target, returned in **nits**
/// (`nits = SDRWhiteLevel * 80 / 1000`). Public + documented API; works whether or
/// not HDR is currently on (the value is only *meaningful* while HDR is on).
pub fn get_sdr_white(t: &TargetId) -> Result<u32, String> {
    unsafe {
        let mut req = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
        req.header = header_for::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(
            DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
            t.adapter,
            t.id,
        );
        let st = DisplayConfigGetDeviceInfo(&mut req.header);
        if st != ERROR_SUCCESS.0 as i32 {
            return Err(format!("GET_SDR_WHITE_LEVEL failed: status {st}"));
        }
        Ok(req.SDRWhiteLevel * 80 / SDR_WHITE_UNITS_PER_80_NITS) // level (1000==80 nits) -> nits
    }
}

/// SET the SDR white level (nits) via the undocumented `SET_SDR_WHITE_LEVEL`
/// (`0xFFFFFFEE`, ledoge). Best-effort: on build 26200 the reverse-engineered path
/// is the only way to programmatically set this (Windows exposes no public SET).
///
/// `nits -> level = nits * 1000 / 80`. GET-before-SET no-ops if already at the
/// requested level (within one quantization unit).
pub fn set_sdr_white(t: &TargetId, nits: u32) -> Result<(), String> {
    // Idempotency: skip if we're already there (avoids a needless driver poke).
    if matches!(get_sdr_white(t), Ok(cur) if cur == nits) {
        log!(
            "set_sdr_white({}, {nits}) -- already {nits} nits, no-op",
            t.name
        );
        return Ok(());
    }
    let level = nits.saturating_mul(SDR_WHITE_UNITS_PER_80_NITS) / 80;
    unsafe {
        let mut req = SetSdrWhiteLevel {
            header: header_for::<SetSdrWhiteLevel>(SET_SDR_WHITE_LEVEL, t.adapter, t.id),
            sdr_white_level: level,
            final_value: 1,
        };
        log!("set_sdr_white({}, {nits} nits) -> level {level}", t.name);
        let st = DisplayConfigSetDeviceInfo(&mut req.header as *const _);
        if st != ERROR_SUCCESS.0 as i32 {
            return Err(format!(
                "SET_SDR_WHITE_LEVEL (0xFFFFFFEE, undocumented) failed: status {st}. \
                 Path is reverse-engineered (ledoge/set_maxtml); the OS exposes no public SET."
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure conversion: level (1000 == 80 nits) -> nits, matching `get_sdr_white`.
    fn level_to_nits(level: u32) -> u32 {
        level * 80 / SDR_WHITE_UNITS_PER_80_NITS
    }
    /// Inverse, matching `set_sdr_white`.
    fn nits_to_level(nits: u32) -> u32 {
        nits * SDR_WHITE_UNITS_PER_80_NITS / 80
    }

    #[test]
    fn nits_conversion_known_points() {
        // level (1000==80 nits) -> nits. 2500=200 / 3125=250 = the ABL-sane band.
        assert_eq!(level_to_nits(1000), 80);
        assert_eq!(level_to_nits(2500), 200);
        assert_eq!(level_to_nits(3125), 250);
        assert_eq!(level_to_nits(6000), 480); // panel peak-ish
    }

    #[test]
    fn nits_roundtrip_is_stable_on_multiples_of_80() {
        for nits in [80u32, 160, 200, 240, 320, 400, 480] {
            assert_eq!(level_to_nits(nits_to_level(nits)), nits, "nits={nits}");
        }
    }

    #[test]
    fn hand_rolled_type_constants() {
        // Guard the hand-rolled values against accidental edits (sources in the
        // module doc): 24H2 GET_ACI2=15, SET_HDR_STATE=16, undocumented white=0xEE.
        assert_eq!(GET_ADVANCED_COLOR_INFO_2.0, 15);
        assert_eq!(SET_HDR_STATE.0, 16);
        assert_eq!(SET_SDR_WHITE_LEVEL.0 as u32, 0xFFFF_FFEE);
    }

    // --- Live tests (authorized display-state testing; run explicitly) ---------
    // Run: cargo test -p recall-hdr --bins -- --ignored --nocapture display::tests
    // (this crate is bin-only, so `--bins` not `--lib`.)

    const MANAGED: &str = "PG27AQDP";

    #[test]
    #[ignore = "live: touches real displays"]
    fn live_enumerate() {
        let all = enumerate_targets().expect("enumerate_targets");
        println!("-- active displays --");
        for (name, t) in &all {
            println!(
                "  '{name}'  adapter=({},{}) id={}",
                t.adapter.LowPart, t.adapter.HighPart, t.id
            );
        }
        assert!(!all.is_empty(), "expected at least one active display");

        let managed = resolve_target(MANAGED).expect("resolve PG27AQDP");
        println!("resolved managed target: '{}'", managed.name());
        assert!(managed.name().eq_ignore_ascii_case(MANAGED));

        // Log + assert the BenQ (if present) is a DIFFERENT target -- we must
        // never touch it. Any non-managed display stands in for that guard.
        let others: Vec<_> = all
            .iter()
            .filter(|(n, _)| !n.eq_ignore_ascii_case(MANAGED))
            .collect();
        for (n, t) in &others {
            println!("other (never touch): '{n}' id={}", t.id);
            assert_ne!(
                &managed, t,
                "a non-managed display resolved to the managed target!"
            );
        }
        if others
            .iter()
            .any(|(n, _)| n.to_ascii_uppercase().contains("BENQ"))
        {
            println!("BenQ present and confirmed != managed target.");
        }
    }

    /// Restores HDR (and SDR-white, if captured) to their originals on drop --
    /// even if the test panics mid-assert. This is the mandatory reset guarantee
    /// for live state-mutating tests.
    struct Restore<'a> {
        t: &'a TargetId,
        hdr: bool,
        nits: Option<u32>,
    }
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            // White level first (may need HDR still on), then HDR.
            if let Some(n) = self.nits {
                let _ = set_sdr_white(self.t, n);
            }
            let _ = set_hdr_state(self.t, self.hdr);
            let now = get_hdr_state(self.t);
            println!(
                "[restore guard] hdr->{} (now {now:?}) white->{:?}",
                self.hdr, self.nits
            );
        }
    }

    #[test]
    #[ignore = "live: flips HDR twice (two screen flashes, authorized)"]
    fn live_hdr_roundtrip() {
        let t = resolve_target(MANAGED).expect("resolve PG27AQDP");
        let original = get_hdr_state(&t).expect("initial GET hdr");
        println!("HDR original = {original}");
        let _guard = Restore {
            t: &t,
            hdr: original,
            nits: None,
        };

        let target = !original;
        set_hdr_state(&t, target).expect("SET hdr to !original");
        std::thread::sleep(std::time::Duration::from_secs(2));
        let mid = get_hdr_state(&t).expect("GET hdr mid");
        println!("HDR after flip = {mid} (wanted {target})");
        assert_eq!(mid, target, "HDR did not flip to the requested state");

        // Explicit restore (the guard is the safety net, not the happy path).
        set_hdr_state(&t, original).expect("SET hdr back to original");
        let after = get_hdr_state(&t).expect("GET hdr after restore");
        println!("HDR after restore = {after} (wanted {original})");
        assert_eq!(after, original, "HDR was not restored to original");
    }

    #[test]
    #[ignore = "live: reads real SDR white level"]
    fn live_sdr_white_get() {
        let t = resolve_target(MANAGED).expect("resolve PG27AQDP");
        let nits = get_sdr_white(&t).expect("GET sdr white");
        println!("SDR white level = {nits} nits");
        assert!(
            (80..=480).contains(&nits),
            "SDR white {nits} outside 80..=480"
        );
    }

    /// Instrument-validity check for the UNDOCUMENTED `SET_SDR_WHITE_LEVEL`
    /// (0xFFFFFFEE): enable HDR, set a different white value, GET-verify it took
    /// (not just "returned Ok"); the guard restores white + HDR on any exit.
    #[test]
    #[ignore = "live: enables HDR + writes SDR white (screen flash, authorized)"]
    fn live_sdr_white_set_roundtrip() {
        let t = resolve_target(MANAGED).expect("resolve PG27AQDP");
        let orig_hdr = get_hdr_state(&t).expect("GET hdr");
        // White level is only meaningful/settable while HDR is on.
        set_hdr_state(&t, true).expect("enable HDR for white-level test");
        std::thread::sleep(std::time::Duration::from_millis(500));
        let orig_nits = get_sdr_white(&t).expect("GET original white");
        println!("original: hdr={orig_hdr} white={orig_nits} nits");
        let _guard = Restore {
            t: &t,
            hdr: orig_hdr,
            nits: Some(orig_nits),
        };

        let want = if orig_nits == 200 { 240 } else { 200 }; // definitely different, ABL-sane
        set_sdr_white(&t, want).expect("SET sdr white (undocumented path)");
        std::thread::sleep(std::time::Duration::from_millis(300));
        let got = get_sdr_white(&t).expect("GET white after set");
        println!("set {want} nits -> GET reads {got} nits");
        assert!(
            (got as i64 - want as i64).abs() <= 1, // 1 quantization unit slack
            "SET_SDR_WHITE_LEVEL did not take: wanted {want}, got {got}"
        );
    }
}
