use super::{planning_dims, upscale_choices, upscaled_dims};
use crate::edit_model::{ExportMode, ExportPriority, ExportSettings};

#[test]
fn a_native_clip_is_never_upscaled() {
    assert_eq!(upscaled_dims(2560, 1440, 1440, 2160), None);
    assert!(upscale_choices(2560, 1440, 1440).is_empty());
}

#[test]
fn a_lower_clip_upscales_up_to_native_and_keeps_its_shape() {
    assert_eq!(upscaled_dims(1920, 1080, 1440, 1440), Some((2560, 1440)));
    assert_eq!(upscaled_dims(1728, 1080, 1200, 1200), Some((1920, 1200)));
    assert_eq!(upscaled_dims(1920, 1080, 1440, 2160), None, "never above native");
    assert_eq!(upscaled_dims(1920, 1080, 1440, 1080), None, "same height is not an upscale");
}

#[test]
fn choices_stop_at_native() {
    assert_eq!(upscale_choices(1280, 720, 1440), vec![1080, 1440]);
    assert_eq!(upscale_choices(1728, 1080, 1200), vec![1200]);
    assert_eq!(upscale_choices(1280, 720, 2160), vec![1080, 1440, 2160]);
}

#[test]
fn planning_upscales_only_when_asked_and_allowed() {
    let ask = ExportSettings { mode: ExportMode::Reencode, upscale_height: Some(1440), ..Default::default() };
    assert_eq!(planning_dims((1920, 1080), Some(1440), &ask), (2560, 1440));
    assert_eq!(planning_dims((1920, 1080), None, &ask), (1920, 1080), "no tag: a native or older clip");
    let lossless = ExportSettings { mode: ExportMode::Lossless, ..ask.clone() };
    assert_eq!(planning_dims((1920, 1080), Some(1440), &lossless), (1920, 1080));
    let fitted = ExportSettings { max_bytes: Some(10_000_000), priority: ExportPriority::Motion, ..ask.clone() };
    assert_eq!(planning_dims((1920, 1080), Some(1440), &fitted), (1920, 1080), "size fits pick their own size");
    assert_eq!(planning_dims((1920, 1080), Some(1440), &ExportSettings::default()), (1920, 1080));
}

#[test]
fn old_projects_load_without_an_upscale() {
    let old: ExportSettings = serde_json::from_str(r#"{"mode":"reencode","max_bytes":null,"height":1080}"#).unwrap();
    assert_eq!(old.upscale_height, None);
    assert!(!serde_json::to_string(&old).unwrap().contains("upscale_height"));
}

#[test]
fn choices_never_pass_the_encoder_width() {
    // A 32:9 clip at 1080p (3840 wide) from a 4320-line source: 4320 would be 15360 wide.
    assert_eq!(upscale_choices(3840, 1080, 4320), vec![1440, 2160]);
    assert_eq!(upscaled_dims(3840, 1080, 4320, 4320), None);
}

#[test]
fn odd_native_heights_offer_only_even_heights_above_the_recording() {
    // A windowed game: recorded at 1016, its window 1017 lines tall.
    assert!(upscale_choices(1806, 1016, 1017).is_empty(), "1016 is the even height under 1017: nothing above the recording");
    assert_eq!(upscale_choices(1280, 720, 1017), vec![1016]);
    assert!(upscale_choices(1280, 720, 1441).iter().all(|h| h % 2 == 0));
}
