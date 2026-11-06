//! Which pixel format a capture starts in, and when a running capture must be replaced for
//! its format. Pure decisions; `CaptureShared::wants_fp16` / `restart_reason` feed them.
//!
//! A capture restart brings a new D3D11 device and frames pool, so the encoder reopens and
//! the replay buffer starts again. The format is therefore picked to survive what can change
//! mid-game: on a display that can turn HDR on, a capture starts in Rgba16F even while the
//! desktop is SDR (fp16 passthrough is the same picture), so HDR turning on needs no restart.
//! The cost: twice the capture-texture memory and bandwidth, and full-frame SDR frames go
//! through the tonemap draw instead of a plain copy. `RECALL_CAPTURE_FP16=0` keeps Bgra8 on
//! such a display (HDR turning on then restarts capture, as before).

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
    /// The primary display can turn HDR on (`CaptureShared::hdr_capable`).
    pub hdr_capable: bool,
}

/// The capture cannot run without fp16: HDR is on, the CPU path or dump harness reads 8-byte
/// pixels, or fp16 is forced.
pub(crate) fn needs_fp16(i: FormatInputs) -> bool {
    !i.gpu_path || i.hdr_on || i.cpu_fallback || i.forced == Some(true)
}

/// The format a start picks: what it needs, plus fp16 ahead of time when HDR can turn on.
pub(crate) fn start_fp16(i: FormatInputs) -> bool {
    needs_fp16(i) || (i.hdr_capable && i.forced != Some(false))
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
pub(crate) fn forced_from_env(value: Option<&std::ffi::OsStr>) -> Option<bool> {
    match value?.to_str()? {
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

    #[test]
    fn an_hdr_capable_display_starts_fp16_while_the_desktop_is_sdr() {
        assert!(!start_fp16(gpu()), "SDR-only display: Bgra8");
        assert!(start_fp16(FormatInputs { hdr_capable: true, ..gpu() }));
        assert!(!start_fp16(FormatInputs { hdr_capable: true, forced: Some(false), ..gpu() }), "opt-out keeps Bgra8");
        assert!(start_fp16(FormatInputs { hdr_on: true, forced: Some(false), ..gpu() }), "HDR on still needs fp16");
    }

    #[test]
    fn only_a_bgra8_capture_that_needs_fp16_restarts() {
        let hdr = FormatInputs { hdr_on: true, ..gpu() };
        assert_eq!(format_restart(false, hdr), Some(HDR_TURNED_ON));
        assert_eq!(format_restart(true, hdr), None, "fp16 capture keeps running when HDR turns on");
        assert_eq!(format_restart(true, gpu()), None, "HDR turning off keeps the fp16 capture");
        assert_eq!(format_restart(false, FormatInputs { hdr_capable: true, ..gpu() }), None, "capability alone never restarts");
        assert_eq!(format_restart(false, FormatInputs { cpu_fallback: true, ..gpu() }), Some(NEEDS_FP16));
    }

    #[test]
    fn env_override_parses_one_and_zero() {
        use std::ffi::OsStr;
        assert_eq!(forced_from_env(Some(OsStr::new("1"))), Some(true));
        assert_eq!(forced_from_env(Some(OsStr::new("0"))), Some(false));
        assert_eq!(forced_from_env(Some(OsStr::new("yes"))), None);
        assert_eq!(forced_from_env(None), None);
    }
}
