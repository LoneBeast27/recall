//! Video bitrate for an export that must finish under a byte cap.
//!
//! Budget: `cap − mux overhead − audio bytes` (audio is encoded before video,
//! so its size is exact). The video encoder then runs VBR with a VBV ceiling:
//! any encoder that honours VBV emits at most `maxrate × duration + bufsize`
//! bits, so `maxrate = budget / (duration + window)` with `bufsize = maxrate ×
//! window` bounds the video stream by the budget. The finished file is still
//! measured, and [`RateControl`] is tightened for a retry if it came out over.

use super::video_encoder::{RateControl, Vbv};

/// Container overhead reserved from the cap (MP4 boxes and sample tables).
pub(crate) const MUX_OVERHEAD_FRACTION: f64 = 0.03;
/// Below this video rate the result would not be watchable.
pub(crate) const MIN_VIDEO_BITRATE: f64 = 100_000.0;
/// Encode attempts before giving up on a cap.
pub(crate) const MAX_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy)]
pub(crate) struct CapInputs {
    pub max_bytes: u64,
    pub duration_s: f64,
    pub audio_bytes: u64,
    pub video_frames: u64,
    pub audio_packets: u64,
    /// The bitrate the export would use without a cap.
    pub quality_bitrate: usize,
}

/// Bytes the container needs beyond the payloads: the larger of 3 % of the cap
/// and an estimate of 16 bytes of index per sample plus 16 KiB of headers.
pub(crate) fn mux_overhead(inputs: &CapInputs) -> u64 {
    let fraction = (inputs.max_bytes as f64 * MUX_OVERHEAD_FRACTION).ceil() as u64;
    let tables = inputs
        .video_frames
        .saturating_add(inputs.audio_packets)
        .saturating_mul(16)
        .saturating_add(16 * 1024);
    fraction.max(tables)
}

fn vbv_window_s(duration_s: f64) -> f64 {
    (duration_s * 0.25).clamp(0.25, 2.0)
}

fn megabytes(bytes: f64) -> String {
    let mb = bytes / 1_000_000.0;
    if mb < 10.0 {
        format!("{mb:.2} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

pub(crate) fn plan_rate(inputs: &CapInputs) -> Result<RateControl, String> {
    let duration = inputs.duration_s;
    if !duration.is_finite() || duration <= 0.0 {
        return Err("the edited export has no duration".into());
    }
    let overhead = mux_overhead(inputs);
    let reserved = overhead.saturating_add(inputs.audio_bytes);
    let window = vbv_window_s(duration);
    let minimum_bytes =
        reserved as f64 + MIN_VIDEO_BITRATE * (duration + window) / 8.0;
    let suggestion = format!(
        "raise the cap to at least {}, shorten the clip, or mute audio tracks",
        megabytes(minimum_bytes / (1.0 - MUX_OVERHEAD_FRACTION))
    );
    if inputs.max_bytes <= reserved {
        return Err(format!(
            "a {} size cap cannot fit this export: its audio alone is {} for {duration:.1} s; {suggestion}",
            megabytes(inputs.max_bytes as f64),
            megabytes(inputs.audio_bytes as f64),
        ));
    }
    let video_bits = (inputs.max_bytes - reserved) as f64 * 8.0;
    let maxrate = video_bits / (duration + window);
    if maxrate < MIN_VIDEO_BITRATE {
        return Err(format!(
            "a {} size cap leaves only {:.0} kbps for {duration:.1} s of video; {suggestion}",
            megabytes(inputs.max_bytes as f64),
            maxrate / 1000.0,
        ));
    }
    Ok(capped(maxrate, window, inputs.quality_bitrate))
}

fn capped(maxrate: f64, window: f64, quality_bitrate: usize) -> RateControl {
    let maxrate = maxrate.floor() as usize;
    RateControl {
        bitrate: quality_bitrate.min(maxrate),
        vbv: Some(Vbv {
            maxrate,
            bufsize: ((maxrate as f64) * window).floor() as usize,
        }),
    }
}

/// Tighter rate for a retry after an attempt produced `actual_bytes > max_bytes`.
pub(crate) fn tighten(rate: RateControl, inputs: &CapInputs, actual_bytes: u64) -> Result<RateControl, String> {
    let vbv = rate.vbv.ok_or("size-capped export lost its rate ceiling")?;
    let ratio = (inputs.max_bytes as f64 / actual_bytes.max(1) as f64) * 0.95;
    let maxrate = vbv.maxrate as f64 * ratio.min(0.95);
    if maxrate < MIN_VIDEO_BITRATE {
        return Err(format!(
            "the export did not fit under {} even at {:.0} kbps; raise the cap or shorten the clip",
            megabytes(inputs.max_bytes as f64),
            vbv.maxrate as f64 / 1000.0
        ));
    }
    Ok(capped(maxrate, vbv_window_s(inputs.duration_s), rate.bitrate.min(maxrate as usize)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(max_bytes: u64) -> CapInputs {
        CapInputs {
            max_bytes,
            duration_s: 30.0,
            audio_bytes: 720_000,
            video_frames: 1800,
            audio_packets: 1400,
            quality_bitrate: 20_000_000,
        }
    }

    #[test]
    fn vbv_bound_fits_the_budget() {
        let cap = 10_000_000;
        let rate = plan_rate(&inputs(cap)).unwrap();
        let vbv = rate.vbv.unwrap();
        let worst_video_bytes = (vbv.maxrate as f64 * 30.0 + vbv.bufsize as f64) / 8.0;
        let total = worst_video_bytes + 720_000.0 + mux_overhead(&inputs(cap)) as f64;
        assert!(total <= cap as f64, "worst case {total} exceeds cap {cap}");
        assert!(rate.bitrate <= vbv.maxrate);
        assert!(vbv.maxrate > 2_000_000, "budget should not be wasted: {vbv:?}");
    }

    #[test]
    fn generous_cap_keeps_the_quality_bitrate() {
        let rate = plan_rate(&inputs(500_000_000)).unwrap();
        assert_eq!(rate.bitrate, 20_000_000);
        assert!(rate.vbv.unwrap().maxrate > 20_000_000);
    }

    #[test]
    fn impossible_caps_explain_themselves() {
        let error = plan_rate(&inputs(500_000)).unwrap_err();
        assert!(error.contains("audio alone"), "{error}");
        let error = plan_rate(&inputs(1_000_000)).unwrap_err();
        assert!(error.contains("kbps"), "{error}");
        assert!(error.contains("raise the cap to at least"), "{error}");
    }

    #[test]
    fn retry_rate_shrinks_by_the_overshoot() {
        let first = plan_rate(&inputs(10_000_000)).unwrap();
        let second = tighten(first, &inputs(10_000_000), 11_000_000).unwrap();
        assert!(second.vbv.unwrap().maxrate < first.vbv.unwrap().maxrate * 9 / 10);
        assert!(second.bitrate <= second.vbv.unwrap().maxrate);
    }
}
