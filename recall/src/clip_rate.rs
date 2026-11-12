//! A clip's recorded frame rate. The recorder writes a variable-rate stream: every captured
//! frame keeps its own time, and a game that dips below the recording rate simply sends
//! fewer frames. The file's average rate (frames / seconds) then reads low (a 60 fps clip
//! with a long dip reads ~50) or high (the capture lets in up to 10% over the rate, so a
//! steady 60 fps clip reads 61-64). The editor caps exports at the clip's rate, so the
//! recorder stores the rate it recorded at in a file-level tag, read here first.
//!
//! Clips saved before the tag carry only the average. A Recall clip's average can never be
//! above 1.1x its recording rate, so the rate is estimated as the smallest common recording
//! rate whose 1.1x cap covers the average. Other apps' files keep their average.

/// File-level MP4 tag (written with `movflags=use_metadata_tags`) holding the rate the clip
/// was recorded at, frames per second as a plain number ("60").
#[allow(dead_code)]
pub const RECORDED_FPS_TAG: &str = "recall_fps";

/// The capture lets in up to this much over the recording rate (`capture::start`).
const CAPTURE_HEADROOM: f64 = 1.1;

/// Recording rates the settings offer, smallest first (the estimate for untagged clips).
const COMMON_RATES: [f64; 6] = [30.0, 60.0, 120.0, 144.0, 165.0, 240.0];

/// The tag's value for a recording at `fps` (None for a rate the tag cannot hold).
#[allow(dead_code)]
pub fn tag_value(num: i32, den: i32) -> Option<String> {
    (num > 0 && den > 0).then(|| {
        let fps = f64::from(num) / f64::from(den);
        if num % den == 0 { (num / den).to_string() } else { format!("{fps:.3}") }
    })
}

/// The tag's rate, when it holds a usable one.
fn parse_tag(tag: Option<&str>) -> Option<f64> {
    tag.and_then(|t| t.trim().parse::<f64>().ok()).filter(|f| f.is_finite() && *f >= 1.0 && *f <= 10_000.0)
}

/// Whether a clip came from the recorder: it has the audio-sources tag, or the recorder's
/// file name (`Game__2026-11-09__23-29-51-776__<32 hex>.mp4`).
#[allow(dead_code)]
pub fn recall_made(path: &std::path::Path, has_sources_tag: bool) -> bool {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let parts: Vec<&str> = stem.split("__").collect();
    has_sources_tag || (parts.len() == 4 && parts[3].len() == 32 && parts[3].bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The clip's frame rate: the recorder's tag; else, for a Recall clip (`recall_made`), the
/// recording rate its average fits; else the file's average rate (0 when unknown).
#[allow(dead_code)]
pub fn clip_fps(tag: Option<&str>, average: f64, recall_made: bool) -> f64 {
    if let Some(fps) = parse_tag(tag) {
        return fps;
    }
    if !(average.is_finite() && average > 0.0) {
        return 0.0;
    }
    if !recall_made {
        return average;
    }
    // A hair of slack for rounding in the average itself.
    COMMON_RATES.iter().copied().find(|rate| average <= rate * CAPTURE_HEADROOM + 0.01).unwrap_or(average)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tag_wins_over_the_average() {
        assert_eq!(clip_fps(Some("60"), 49.7, true), 60.0);
        assert_eq!(clip_fps(Some(" 144 "), 160.0, false), 144.0);
        assert_eq!(clip_fps(Some("59.940"), 52.0, true), 59.94);
    }

    #[test]
    fn a_bad_tag_counts_as_none() {
        assert_eq!(clip_fps(Some("fast"), 30.0, false), 30.0);
        assert_eq!(clip_fps(Some("0"), 64.0, true), 60.0);
        assert_eq!(clip_fps(Some("inf"), 64.0, true), 60.0);
    }

    #[test]
    fn old_recall_clips_read_their_recording_rate() {
        // Measured on real clips recorded at 60: Apex 60.6, VALORANT 64.0 average.
        assert_eq!(clip_fps(None, 60.63, true), 60.0);
        assert_eq!(clip_fps(None, 63.97, true), 60.0);
        // A long dip (a game at 40 fps for half of a 60 fps clip).
        assert_eq!(clip_fps(None, 50.0, true), 60.0);
        assert_eq!(clip_fps(None, 66.0, true), 60.0);
        assert_eq!(clip_fps(None, 70.0, true), 120.0);
        assert_eq!(clip_fps(None, 140.0, true), 144.0);
        assert_eq!(clip_fps(None, 28.0, true), 30.0);
        assert_eq!(clip_fps(None, 300.0, true), 300.0, "above every cap: the average stands");
    }

    #[test]
    fn other_files_keep_their_average() {
        assert_eq!(clip_fps(None, 25.0, false), 25.0);
        assert_eq!(clip_fps(None, 59.94, false), 59.94);
        assert_eq!(clip_fps(None, 0.0, true), 0.0);
        assert_eq!(clip_fps(None, f64::NAN, true), 0.0);
    }

    #[test]
    fn recorder_clips_by_name_or_tag() {
        use std::path::Path;
        assert!(recall_made(Path::new(r"V:\Raw\VALORANT__2026-11-09__23-29-51-776__238300f93e7b46f5adceb5f3a8d22699.mp4"), false));
        assert!(!recall_made(Path::new(r"V:\Raw\Apex-Legends__2022-10-21__23-37-23.mp4"), false));
        assert!(recall_made(Path::new("clip.mp4"), true));
        assert!(!recall_made(Path::new("a__b__c__nothex.mp4"), false));
    }

    #[test]
    fn tag_values() {
        assert_eq!(tag_value(60, 1).as_deref(), Some("60"));
        assert_eq!(tag_value(120, 2).as_deref(), Some("60"));
        assert_eq!(tag_value(60000, 1001).as_deref(), Some("59.940"));
        assert_eq!(tag_value(0, 1), None);
        assert_eq!(tag_value(60, 0), None);
    }
}
