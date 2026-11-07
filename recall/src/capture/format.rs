//! Which pixel format a capture starts in, and when a running capture must be replaced for
//! its format. Pure decisions; `CaptureShared::wants_fp16` / `restart_reason` feed them.
//!
//! A capture restart brings a new D3D11 device and frames pool, so the encoder reopens and
//! the replay buffer starts again. The format is therefore picked to survive what can change
//! mid-game: on a display that can turn HDR on, a capture starts in Rgba16F even while the
//! desktop is SDR (fp16 passthrough is the same picture), so HDR turning on needs no restart.
//! The cost, measured at 1440p against Bgra8 (`.tmp/recall-wave-b/hdr-ab-gpu-1440.log`):
//! +0.02 pp 3D GPU load, no CPU change, +30-45 MB VRAM. A card with 4 GiB of VRAM or less
//! keeps Bgra8 (HDR turning on then restarts capture and the controller says so), and so
//! does a card whose VRAM could not be read. `RECALL_CAPTURE_FP16=0` keeps Bgra8, `=1`
//! forces fp16, whatever the display and the card.

/// `restart_reason` when HDR turned on under a Bgra8 capture.
pub(crate) const HDR_TURNED_ON: &str = "in Rgba16F (HDR turned on)";
/// `restart_reason` when a Bgra8 frame needs the CPU path (or fp16 was forced).
pub(crate) const NEEDS_FP16: &str = "in Rgba16F (CPU fallback)";

/// What the format decision reads.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FormatInputs {
    /// The zero-copy GPU path can run (`gpu_route::gpu_path_possible`).
    pub gpu_path: bool,
    /// The display is HDR (tonemap mode).
    pub hdr_on: bool,
    /// A Bgra8 frame could not take the GPU path (`CaptureShared::need_fp16`).
    pub cpu_fallback: bool,
    /// `RECALL_CAPTURE_FP16`: `Some(true)` = "1", `Some(false)` = "0".
    pub forced: Option<bool>,
}

/// The capture cannot run without fp16: HDR is on, the CPU path or dump harness reads 8-byte
/// pixels, or fp16 is forced.
pub(crate) fn needs_fp16(i: FormatInputs) -> bool {
    !i.gpu_path || i.hdr_on || i.cpu_fallback || i.forced == Some(true)
}

/// Cards with this much dedicated VRAM or less keep Bgra8 ahead of time.
pub(crate) const SMALL_VRAM: u64 = 4 * 1024 * 1024 * 1024;

/// Whether a start picks fp16 ahead of time (on top of [`needs_fp16`]), so HDR turning on
/// mid-game needs no restart: an HDR-capable display on a card with more than 4 GiB of VRAM.
/// `vram_bytes` is the capture adapter's dedicated VRAM; `None` (not read) keeps Bgra8, as
/// a small card would. `env` is `RECALL_CAPTURE_FP16`: "1" forces fp16 (also on an SDR-only
/// display, as before this rule), "0" keeps Bgra8; any other value is ignored.
pub(crate) fn start_fp16(hdr_capable: bool, vram_bytes: Option<u64>, env: Option<&str>) -> bool {
    match forced_from_env(env) {
        Some(forced) => forced,
        None => hdr_capable && vram_bytes.is_some_and(|v| v > SMALL_VRAM),
    }
}

/// Why a running capture must be replaced for its format: only a Bgra8 capture that now
/// needs fp16. A capture already fp16 is never restarted for its format (HDR turning off
/// is passthrough of the same fp16 frames).
pub(crate) fn format_restart(running_fp16: bool, i: FormatInputs) -> Option<&'static str> {
    if running_fp16 || !needs_fp16(i) {
        return None;
    }
    Some(if i.hdr_on { HDR_TURNED_ON } else { NEEDS_FP16 })
}

/// `RECALL_CAPTURE_FP16` as `FormatInputs::forced`.
pub(crate) fn forced_from_env(value: Option<&str>) -> Option<bool> {
    match value? {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu() -> FormatInputs {
        FormatInputs { gpu_path: true, ..Default::default() }
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn an_hdr_capable_display_starts_fp16_while_the_desktop_is_sdr() {
        assert!(!needs_fp16(gpu()), "an SDR desktop on the GPU path needs no fp16");
        assert!(start_fp16(true, Some(8 * GIB), None), "fp16 ahead of time");
        assert!(needs_fp16(FormatInputs { hdr_on: true, forced: Some(false), ..gpu() }), "HDR on still needs fp16 under =0");
    }

    #[test]
    fn a_4_gib_card_keeps_bgra8_and_a_6_gib_card_starts_fp16() {
        assert!(!start_fp16(true, Some(4 * GIB), None), "exactly 4 GiB: no fp16 ahead of time");
        assert!(!start_fp16(true, Some(2 * GIB), None), "2 GiB: no fp16 ahead of time");
        assert!(start_fp16(true, Some(4 * GIB + 1), None), "just over 4 GiB: fp16");
        assert!(start_fp16(true, Some(6 * GIB), None), "6 GiB: fp16");
    }

    #[test]
    fn the_env_override_wins_over_the_vram_rule() {
        assert!(!start_fp16(true, Some(6 * GIB), Some("0")), "=0 on 6 GiB: Bgra8");
        assert!(start_fp16(true, Some(4 * GIB), Some("1")), "=1 on 4 GiB: fp16");
        assert!(start_fp16(false, None, Some("1")), "=1 forces fp16 on an SDR-only display too");
        assert!(start_fp16(true, Some(6 * GIB), Some("yes")), "an unknown value is ignored");
    }

    #[test]
    fn an_sdr_only_display_or_unknown_vram_keeps_bgra8() {
        assert!(!start_fp16(false, Some(24 * GIB), None), "not HDR-capable: Bgra8");
        assert!(!start_fp16(true, None, None), "VRAM unknown: Bgra8 (restart + notice on HDR on)");
    }

    #[test]
    fn only_a_bgra8_capture_that_needs_fp16_restarts() {
        let hdr = FormatInputs { hdr_on: true, ..gpu() };
        assert_eq!(format_restart(false, hdr), Some(HDR_TURNED_ON));
        assert_eq!(format_restart(true, hdr), None, "fp16 capture keeps running when HDR turns on");
        assert_eq!(format_restart(true, gpu()), None, "HDR turning off keeps the fp16 capture");
        assert_eq!(format_restart(false, gpu()), None, "an SDR Bgra8 capture keeps running");
        assert_eq!(format_restart(false, FormatInputs { cpu_fallback: true, ..gpu() }), Some(NEEDS_FP16));
    }

    #[test]
    fn env_override_parses_one_and_zero() {
        assert_eq!(forced_from_env(Some("1")), Some(true));
        assert_eq!(forced_from_env(Some("0")), Some(false));
        assert_eq!(forced_from_env(Some("yes")), None);
        assert_eq!(forced_from_env(None), None);
    }
}
