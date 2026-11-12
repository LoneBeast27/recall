//! Size rules for upscaled export. Pure arithmetic, no GPU.

/// Even sides, at least 2 (4:2:0 chroma needs even sizes).
fn even(value: u32) -> u32 {
    (value & !1).max(2)
}

/// Output size for an upscaled export, or None when the rule forbids it:
/// the clip must be recorded below `native_height`, and the target must sit
/// above the recorded height and at or below native. Keeps the clip's shape.
pub(crate) fn upscaled_dims(width: u32, height: u32, native_height: u32, target_height: u32) -> Option<(u32, u32)> {
    if height == 0 || height >= native_height || target_height <= height || target_height > native_height {
        return None;
    }
    let out_height = even(target_height);
    let out_width = (width as f64 * out_height as f64 / height as f64).round() as u32;
    Some((even(out_width + (out_width & 1)), out_height))
}

/// The size an export plans from: the upscaled size when the settings ask for one the clip
/// allows (re-encode, manual priority), else the recorded size.
pub(crate) fn planning_dims(recorded: (u32, u32), native_height: Option<u32>, settings: &crate::edit_model::ExportSettings) -> (u32, u32) {
    let manual = settings.active_priority() == crate::edit_model::ExportPriority::Manual;
    let reencode = settings.mode == crate::edit_model::ExportMode::Reencode;
    match (settings.upscale_height, native_height) {
        (Some(target), Some(native)) if manual && reencode => upscaled_dims(recorded.0, recorded.1, native, target).unwrap_or(recorded),
        _ => recorded,
    }
}

/// The export panel's list lives with the settings it fills (shared with the app).
pub(crate) use crate::edit_model::upscale_choices;
