use super::{capped, copy_rect, crop_rgba, fit, Layout, Rect, MIN_ENCODE};

/// A 1440p canvas: a borderless 1440p session that later showed a title bar.
const CANVAS: (u32, u32) = (2560, 1440);

#[test]
fn the_observed_windowed_game_is_letterboxed_unscaled() {
    // sobosuba, maximized with a caption: client 2560x1369 inside a 2560x1392 frame.
    let l = Layout::on_canvas(Rect { x: 0, y: 23, w: 2560, h: 1369 }, CANVAS);
    assert_eq!(l.out, (2560, 1440));
    assert_eq!(l.src, Rect { x: 0, y: 23, w: 2560, h: 1369 });
    // 71 spare rows: 34 on top (even), 37 below; same size = pixel-exact copy.
    assert_eq!(l.dst, Rect { x: 0, y: 34, w: 2560, h: 1369 });
    assert_eq!(l.content(), Some(l.dst));
}

#[test]
fn small_content_is_centred_and_never_upscaled() {
    assert_eq!(fit((1440, 810), CANVAS), Rect { x: 560, y: 314, w: 1440, h: 810 });
    assert_eq!(fit((800, 1400), CANVAS), Rect { x: 880, y: 20, w: 800, h: 1400 });
    assert_eq!(fit((2560, 1440), CANVAS), Rect::full(CANVAS));
    assert_eq!(fit((1, 1), CANVAS), Rect { x: 1278, y: 718, w: 1, h: 1 });
}

#[test]
fn larger_content_scales_down_with_its_aspect() {
    // 4K client: exactly 2/3.
    assert_eq!(fit((3840, 2160), CANVAS), Rect::full(CANVAS));
    // Ultrawide 3440x1400: width-bound, 1042 rows (1041.9), centred vertically.
    assert_eq!(fit((3440, 1400), CANVAS), Rect { x: 0, y: 198, w: 2560, h: 1042 });
    // Tall 2000x2000: height-bound.
    assert_eq!(fit((2000, 2000), CANVAS), Rect { x: 560, y: 0, w: 1440, h: 1440 });
    // One axis over, the other under: still scaled to fit.
    assert_eq!(fit((2600, 1000), CANVAS), Rect { x: 0, y: 226, w: 2560, h: 985 });
}

#[test]
fn every_fit_stays_inside_the_canvas() {
    for (w, h) in [(1, 9999), (9999, 1), (2561, 1441), (7680, 4320), (2559, 1439), (3000, 1440)] {
        let r = fit((w, h), CANVAS);
        assert!(r.x + r.w <= CANVAS.0 && r.y + r.h <= CANVAS.1, "{w}x{h} -> {r:?}");
        assert!(r.w >= 1 && r.h >= 1 && r.x % 2 == 0 && r.y % 2 == 0, "{w}x{h} -> {r:?}");
        assert!(r.w <= w && r.h <= h, "never upscaled: {w}x{h} -> {r:?}");
    }
}

#[test]
fn native_layout_crops_or_pads_without_scaling() {
    let l = Layout::native((2566, 1434), (2560, 1440));
    assert_eq!((l.src, l.dst, l.out), (Rect::full((2560, 1434)), Rect::full((2560, 1434)), (2560, 1440)));
    assert_eq!(l.content(), None);
}

#[test]
fn clamp_keeps_the_part_inside_the_frame() {
    let r = Rect { x: 10, y: 20, w: 100, h: 100 };
    assert_eq!(r.clamp_to((50, 200)), Some(Rect { x: 10, y: 20, w: 40, h: 100 }));
    assert_eq!(r.clamp_to((10, 200)), None);
}

#[test]
fn copy_rect_honours_stride_and_pixel_size() {
    // 4x3 image, 2 bytes per pixel, 10-byte rows (2 bytes of padding): byte = 10*y + x*2 + c.
    let data: Vec<u8> = (0..30).collect();
    let out = copy_rect(&data, 10, 2, Rect { x: 1, y: 1, w: 2, h: 2 });
    assert_eq!(out, vec![12, 13, 14, 15, 22, 23, 24, 25]);
    let (w, h, px) = crop_rgba(2, &[1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4], Rect { x: 1, y: 0, w: 1, h: 2 });
    assert_eq!((w, h, px), (1, 2, vec![2, 2, 2, 2, 4, 4, 4, 4]));
}

#[test]
fn a_frame_too_small_for_nvenc_renders_nothing_on_its_own_size() {
    let tiny = (160, 28);
    assert_eq!(Layout::recorded(tiny, None, None, Some((2560, 1440))), None, "the frames pool must not change");
    assert_eq!(Layout::recorded(tiny, None, None, None), None, "nor open on it");
    let canvas = Layout::recorded(tiny, None, Some(CANVAS), Some(CANVAS)).expect("the canvas has a fixed size");
    assert_eq!(canvas.out, CANVAS);
    let wide = (2560, MIN_ENCODE.1 - 2);
    assert_eq!(Layout::recorded(wide, None, None, None), None, "either axis counts");
    assert!(Layout::recorded(MIN_ENCODE, None, None, None).is_some());
}

#[test]
fn a_height_cap_keeps_the_source_shape_and_never_upscales() {
    assert_eq!(capped((2560, 1440), Some(1080)), (1920, 1080));
    assert_eq!(capped((1920, 1200), Some(1080)), (1728, 1080), "16:10 stays 16:10");
    assert_eq!(capped((2560, 1600), Some(1440)), (2304, 1440));
    assert_eq!(capped((3440, 1440), Some(720)), (1720, 720), "ultrawide stays ultrawide");
    assert_eq!(capped((1600, 900), Some(1080)), (1600, 900), "below the cap: native");
    assert_eq!(capped((1600, 900), Some(720)), (1280, 720));
    assert_eq!(capped((1366, 768), Some(720)), (1280, 720), "1280.6 -> even 1280");
    assert_eq!(capped((2560, 1440), None), (2560, 1440));
}

#[test]
fn a_capped_recording_scales_the_whole_frame_with_no_bars() {
    let l = Layout::recorded_at((2560, 1600), None, None, None, Some(1080)).expect("encodable");
    assert_eq!((l.src, l.dst, l.out), (Rect::full((2560, 1600)), Rect::full((1728, 1080)), (1728, 1080)));
    assert_eq!(l.content(), None);
    // Border jitter on a scaled session keeps the encoder size.
    let j = Layout::recorded_at((2560, 1598), None, None, Some(l.out), Some(1080)).expect("encodable");
    assert_eq!(j.out, (1728, 1080));
    // Under the cap: the unscaled native layout, as before.
    let n = Layout::recorded_at((1600, 900), None, None, None, Some(1080)).expect("encodable");
    assert_eq!(n, Layout::native((1600, 900), (1600, 900)));
    // A windowed game: its canvas shrinks in its own shape, the picture fills it.
    let crop = Rect { x: 1, y: 31, w: 1920, h: 1200 };
    let w = Layout::recorded_at((1922, 1232), Some(crop), Some((1920, 1200)), None, Some(720)).expect("canvas");
    assert_eq!((w.out, w.dst), ((1152, 720), Rect::full((1152, 720))));
}
