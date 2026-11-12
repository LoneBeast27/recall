use super::{upscale_choices, upscaled_dims};

#[test]
fn a_native_clip_is_never_upscaled() {
    assert_eq!(upscaled_dims(2560, 1440, 1440, 2160), None);
    assert!(upscale_choices(1440, 1440).is_empty());
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
    assert_eq!(upscale_choices(720, 1440), vec![1080, 1440]);
    assert_eq!(upscale_choices(1080, 1200), vec![1200]);
    assert_eq!(upscale_choices(720, 2160), vec![1080, 1440, 2160]);
}
