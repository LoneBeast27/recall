//! Whether a pre-encode may run: its file goes into the cache, so it counts against the cache
//! budget, and the cache drive keeps 2 GiB free beyond it (the recorder stages clips there).
//! When it does not fit, the pre-encode is skipped and Export simply encodes as usual.
use crate::edit_model::{ClipInfo, EditPlan, ExportSettings};

/// Free space the cache drive keeps beyond the prepared file.
pub const FREE_MARGIN: u64 = 2 << 30;
/// AAC per output track, as the engine encodes it (96 kbps per channel, stereo).
const AUDIO_BITS_PER_TRACK: u64 = 192_000;

/// The size the prepared file will be: the size limit when one is set, else the engine's
/// quality bitrate (0.1 bit per output pixel, 4–120 Mbps) plus the audio, over the output length.
pub fn estimate_bytes(info: &ClipInfo, plan: &EditPlan, fps: u32, export: &ExportSettings) -> u64 {
    let seconds: f64 = plan.ranges.iter().map(|r| (r.end - r.start).max(0.0) / if r.speed > 0.0 { r.speed } else { 1.0 }).sum();
    if let Some(cap) = export.max_bytes {
        return cap;
    }
    let (width, height) = output_dims(info.width, info.height, export.height);
    let video = (u64::from(width) * u64::from(height) * u64::from(fps) / 10).clamp(4_000_000, 120_000_000);
    let tracks = info.audio.len().max(1) as u64;
    ((video + AUDIO_BITS_PER_TRACK * tracks) as f64 * seconds / 8.0).ceil() as u64
}

/// The engine's output size (`export::video_encoder::output_dims`): never upscales, keeps the
/// aspect, both sides even.
pub fn output_dims(width: u32, height: u32, target_height: Option<u32>) -> (u32, u32) {
    let even = |value: u32| (value & !1).max(2);
    match target_height {
        Some(target) if target < height => {
            let out_height = even(target);
            let out_width = (width as f64 * out_height as f64 / height as f64).round() as u32;
            (even(out_width + (out_width & 1)), out_height)
        }
        _ => (even(width), even(height)),
    }
}

/// The prepared file fits: within the budget left (`budget` minus `used`) and with
/// [`FREE_MARGIN`] still free after it. Unknown free space never fits.
pub fn fits(estimate: u64, free: Option<u64>, budget: u64, used: u64) -> bool {
    let free_after = free.and_then(|free| free.checked_sub(estimate));
    free_after.is_some_and(|left| left >= FREE_MARGIN) && used.saturating_add(estimate) <= budget
}

/// [`fits`] against the app's cache now (walks the cache: call it off the UI thread).
pub fn room_for(estimate: u64) -> bool {
    let usage = crate::services::storage::ssd_usage(&crate::backend::library_dir());
    fits(estimate, usage.free, usage.budget, usage.used())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit_model::TimeRange;

    const GIB: u64 = 1 << 30;

    #[test]
    fn a_pre_encode_needs_budget_room_and_two_gib_free_after_it() {
        assert!(fits(GIB / 2, Some(10 * GIB), 2 * GIB, GIB), "half a GiB into a 2 GiB budget with 1 GiB used");
        assert!(!fits(GIB + 1, Some(10 * GIB), 2 * GIB, GIB), "over the budget left");
        assert!(!fits(GIB / 2, Some(2 * GIB + GIB / 4), 100 * GIB, 0), "less than 2 GiB free after it");
        assert!(fits(GIB / 2, Some(2 * GIB + GIB / 2), 100 * GIB, 0), "exactly 2 GiB free after it");
        assert!(!fits(1, None, 100 * GIB, 0), "free space unknown");
        assert!(!fits(GIB, Some(GIB / 2), 100 * GIB, 0), "bigger than the drive's free space");
    }

    fn clip(width: u32, height: u32) -> ClipInfo {
        ClipInfo { path: "E:/a.mp4".into(), duration: 3600.0, width, height, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new() }
    }

    #[test]
    fn the_estimate_follows_length_resolution_and_size_limit() {
        let plan = |end: f64| EditPlan { mix_down: None, ranges: vec![TimeRange::new(0.0, end)], audio: Vec::new() };
        let settings = ExportSettings::default();
        // 10 min of 4K60: 49.8 Mbps video + one AAC track, about 3.7 GB.
        let uhd = estimate_bytes(&clip(3840, 2160), &plan(600.0), 60, &settings);
        assert!((3_700_000_000..3_760_000_000).contains(&uhd), "{uhd}");
        assert!(!fits(uhd, Some(500 * GIB), 2 * GIB, 0), "a 10-min 4K cut never fits the 2 GiB C: budget");
        let small = estimate_bytes(&clip(3840, 2160), &plan(600.0), 60, &ExportSettings { height: Some(720), ..settings.clone() });
        assert!(small < uhd / 4, "720p is far smaller: {small}");
        let capped = estimate_bytes(&clip(3840, 2160), &plan(600.0), 60, &ExportSettings { max_bytes: Some(10_000_000), ..settings });
        assert_eq!(capped, 10_000_000);
        let slow = EditPlan { mix_down: None, ranges: vec![TimeRange { start: 0.0, end: 60.0, speed: 0.5 }], audio: Vec::new() };
        assert_eq!(estimate_bytes(&clip(1920, 1080), &slow, 60, &ExportSettings::default()), estimate_bytes(&clip(1920, 1080), &plan(120.0), 60, &ExportSettings::default()));
    }
}
