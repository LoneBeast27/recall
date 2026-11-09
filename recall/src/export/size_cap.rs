//! Video bitrate for an export that must finish under a byte cap.
//!
//! Budget: `cap − mux overhead − audio bytes` (audio is encoded before video,
//! so its size is exact). The video encoder then runs VBR with a VBV ceiling:
//! any encoder that honours VBV emits at most `maxrate × duration + bufsize`
//! bits, so `maxrate = budget / (duration + window)` with `bufsize = maxrate ×
//! window` bounds the video stream by the budget. The finished file is still
//! measured, and [`RateControl`] is tightened for a retry if it came out over.

use super::timeline::{ticks_before, us_to_seconds};
use super::video_encoder::{output_dims, recommended_bitrate, RateControl, Vbv, HEVC_NVENC_MAX};
use crate::edit_model::{ExportPriority, ExportSettings, MIN_EXPORT_HEIGHT};

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

pub(crate) struct VideoPlan {
    pub dims: (u32, u32),
    pub fps: u32,
    pub rate: RateControl,
    pub cap: Option<CapInputs>,
}

/// Use the exact audio payload and the sample count at each candidate frame rate.
pub(crate) fn plan_video(
    source_dims: (u32, u32),
    max_fps: u32,
    duration_us: i64,
    (audio_bytes, audio_packets): (u64, u64),
    settings: &ExportSettings,
) -> Result<VideoPlan, String> {
    let bounds = output_dims(source_dims.0, source_dims.1, settings.output_height_limit());
    let Some(max_bytes) = settings.max_bytes else {
        return Ok(VideoPlan {
            dims: bounds, fps: max_fps, cap: None,
            rate: RateControl::quality(recommended_bitrate(bounds.0, bounds.1, max_fps)),
        });
    };

    let priority = settings.active_priority();
    // Under Motion/Detail the Resolution control is off: the fit itself stays within NVENC.
    let bounds = if priority == ExportPriority::Manual { bounds } else { within_encoder(source_dims, bounds) };
    let plan_at = |fps| -> Result<VideoPlan, String> {
        let inputs = CapInputs {
            max_bytes,
            duration_s: us_to_seconds(duration_us),
            audio_bytes,
            video_frames: ticks_before(duration_us, fps),
            audio_packets,
            quality_bitrate: recommended_bitrate(bounds.0, bounds.1, fps),
        };
        let mut rate = plan_rate(&inputs)?;
        let dims = match priority {
            ExportPriority::Manual => bounds,
            _ => fit_dimensions(source_dims, bounds, fps, rate.bitrate),
        };
        rate.bitrate = rate.bitrate.min(recommended_bitrate(dims.0, dims.1, fps));
        Ok(VideoPlan { dims, fps, rate, cap: Some(inputs) })
    };

    let mut best = plan_at(max_fps);
    if priority == ExportPriority::Detail {
        for fps in [60, 30].into_iter().filter(|fps| *fps < max_fps) {
            if let Ok(candidate) = plan_at(fps) {
                // Ties keep the higher frame rate. The candidates are tried in descending order.
                if best.as_ref().map_or(true, |current| candidate.dims.1 > current.dims.1) {
                    best = Ok(candidate);
                }
            }
        }
    }
    best
}

/// `bounds` scaled down (aspect kept, even sides) until its long side fits NVENC's largest
/// frame ([`HEVC_NVENC_MAX`]); unchanged when it already fits.
fn within_encoder(source: (u32, u32), bounds: (u32, u32)) -> (u32, u32) {
    let long = bounds.0.max(bounds.1);
    if long <= HEVC_NVENC_MAX {
        return bounds;
    }
    let mut height = (u64::from(bounds.1) * u64::from(HEVC_NVENC_MAX) / u64::from(long)) as u32 & !1;
    loop {
        let dims = output_dims(source.0, source.1, Some(height));
        if dims.0.max(dims.1) <= HEVC_NVENC_MAX || height <= 2 {
            return dims;
        }
        height -= 2;
    }
}

/// A quality heuristic for fast motion: at least 0.06 bits per pixel per frame.
/// Keep the largest common tier within budget. A very tight cap can reach the existing
/// minimum export height, where the hard size check still applies.
fn fit_dimensions(source: (u32, u32), bounds: (u32, u32), fps: u32, bitrate: usize) -> (u32, u32) {
    let adequate = |(w, h): (u32, u32)| {
        bitrate as u128 * 100 >= u128::from(w) * u128::from(h) * u128::from(fps) * 6
    };
    if adequate(bounds) {
        return bounds;
    }
    [2160, 1440, 1080, 900, 720, 540, 480, 360, 240]
        .into_iter()
        .filter(|height| *height < bounds.1)
        .map(|height| output_dims(source.0, source.1, Some(height)))
        .find(|dims| adequate(*dims))
        .unwrap_or_else(|| output_dims(source.0, source.1, Some(MIN_EXPORT_HEIGHT.min(bounds.1))))
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
    fn priorities_fit_different_shapes_to_the_same_budget() {
        let plan = |priority, cap| plan_video(
            (2560, 1440), 60, 30_000_000, (1_440_000, 2800),
            &ExportSettings { max_bytes: Some(cap), height: Some(720), priority, ..ExportSettings::default() },
        ).unwrap();
        let manual = plan(ExportPriority::Manual, 10_000_000);
        assert_eq!((manual.dims, manual.fps), ((1280, 720), 60));
        let motion = plan(ExportPriority::Motion, 10_000_000);
        assert_eq!((motion.dims, motion.fps), ((960, 540), 60));
        let detail = plan(ExportPriority::Detail, 10_000_000);
        assert_eq!((detail.dims, detail.fps), ((1280, 720), 30));
        assert_eq!(detail.cap.unwrap().video_frames, 900);
        let roomy = plan(ExportPriority::Detail, 500_000_000);
        assert_eq!((roomy.dims, roomy.fps), ((2560, 1440), 60));
    }

    #[test]
    fn a_fitted_size_over_the_encoder_limit_is_clamped_not_refused() {
        // 4fcc5d2 review: under Motion/Detail the Resolution control is greyed out, so a fit
        // must never hand NVENC more than it takes (8192 px a side).
        for priority in [ExportPriority::Motion, ExportPriority::Detail] {
            for source in [(10240, 1440), (1440, 10240), (16384, 4320)] {
                let plan = plan_video(source, 60, 30_000_000, (1_440_000, 2800),
                    &ExportSettings { max_bytes: Some(4_000_000_000), priority, ..ExportSettings::default() }).unwrap();
                let (w, h) = plan.dims;
                assert!(w.max(h) <= 8192 && w % 2 == 0 && h % 2 == 0, "{priority:?} {source:?} -> {:?}", plan.dims);
                assert!(super::super::video_encoder::nvenc_codec(w, h).is_ok(), "{priority:?} {source:?}: no error");
            }
        }
        // Any error that remains (Original, no priority) names no control that can be disabled.
        let error = super::super::video_encoder::nvenc_codec(10240, 1440).unwrap_err();
        assert!(!error.contains("under Resolution"), "{error}");
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
