//! Editor visuals (poster + 8-frame filmstrip) in the SSD frame tier
//! (`<cache>/visuals/<identity>/`, shared with hover-scrub frames). Keyed by the original
//! clip, extracted from whichever copy is cheapest to read (the SSD video copy after an open).
use std::path::Path;

use crate::backend::VisualAssets;
use crate::services::storage::{self, Seek};

const FRAME_COUNT: usize = 8;
const WIDTH: u32 = 320;

/// The poster and filmstrip already cached for `original`, extracting nothing.
pub(crate) fn cached(original: &Path) -> VisualAssets {
    let Ok((id, _, _)) = storage::identity_of(original) else { return VisualAssets::default() };
    let directory = storage::visuals_dir(&storage::cache_root(), &id);
    let found = |label: String| Some(directory.join(format!("{label}.jpg"))).filter(|p| storage::valid_file(p)).map(|p| p.to_string_lossy().into_owned());
    let filmstrip: Option<Vec<String>> = (0..FRAME_COUNT).map(|index| found(format!("frame-{index}"))).collect();
    VisualAssets { poster: found("poster".into()), filmstrip: filmstrip.unwrap_or_default() }
}

/// The successful frame paths are useful even if a later representative seek fails.
pub(crate) fn visuals_best_effort(original: &Path, read_from: &Path, source_duration: f64) -> (VisualAssets, Option<String>) {
    let id = match storage::identity_of(original) {
        Ok((id, _, _)) => id,
        Err(error) => return (VisualAssets::default(), Some(format!("Visual previews: {error}"))),
    };
    let directory = storage::visuals_dir(&storage::cache_root(), &id);
    if let Err(error) = std::fs::create_dir_all(&directory) {
        return (VisualAssets::default(), Some(format!("Visual previews: create cache: {error}")));
    }
    let lowered = storage::drive_kind(read_from).is_hdd();
    let duration = if source_duration.is_finite() && source_duration > 0.0 {
        source_duration
    } else {
        match storage::probe_duration(read_from, lowered) {
            Ok(value) => value,
            Err(error) => return (VisualAssets::default(), Some(error)),
        }
    };
    let frame = |label: &str, seconds: f64| -> Result<String, String> {
        let target = directory.join(format!("{label}.jpg"));
        if !storage::valid_file(&target) {
            storage::extract_frame(read_from, seconds, WIDTH, 4, &target, Seek::Exact, lowered)?;
        }
        Ok(target.to_string_lossy().into_owned())
    };
    let mut warnings = Vec::new();
    let poster = match frame("poster", duration * 0.05) {
        Ok(value) => Some(value),
        Err(error) => {
            warnings.push(format!("poster: {error}"));
            None
        }
    };
    let mut filmstrip = Vec::with_capacity(FRAME_COUNT);
    for index in 0..FRAME_COUNT {
        let seconds = duration * ((index as f64 + 0.5) / FRAME_COUNT as f64);
        match frame(&format!("frame-{index}"), seconds) {
            Ok(value) => filmstrip.push(value),
            Err(error) => warnings.push(format!("frame {}: {error}", index + 1)),
        }
    }
    let warning = if warnings.is_empty() { None } else { Some(format!("Visual previews: {}", warnings.join("; "))) };
    (VisualAssets { poster, filmstrip }, warning)
}
