use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{POINT, RECT};

use super::{client_in_bounds, Client, ClientArea, REPROBE};
use crate::framing::{Layout, Rect};

fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT { left: l, top: t, right: r, bottom: b }
}

#[test]
fn maximized_captioned_window_has_its_title_bar_cropped() {
    // The observed case: bounds 0,0 -> 2560,1392; client 2560x1369 at screen (0, 23).
    let c = client_in_bounds(rect(0, 0, 2560, 1392), POINT { x: 0, y: 23 }, (2560, 1369));
    assert_eq!(c, Client::Inset(Rect { x: 0, y: 23, w: 2560, h: 1369 }));
}

#[test]
fn normal_window_crops_caption_and_borders() {
    // A restored window at (560, 315) with a 31 px caption and 1 px borders.
    let c = client_in_bounds(rect(560, 315, 2002, 1157), POINT { x: 561, y: 346 }, (1440, 810));
    assert_eq!(c, Client::Inset(Rect { x: 1, y: 31, w: 1440, h: 810 }));
}

#[test]
fn borderless_and_fullscreen_windows_fill_their_frame() {
    assert_eq!(client_in_bounds(rect(0, 0, 2560, 1440), POINT { x: 0, y: 0 }, (2560, 1440)), Client::Fills);
    // Off-screen origin (the engine-cost target) is still a fill.
    assert_eq!(client_in_bounds(rect(-2559, 1439, 1, 2879), POINT { x: -2559, y: 1439 }, (2560, 1440)), Client::Fills);
}

#[test]
fn unmeasurable_windows_are_unknown() {
    assert_eq!(client_in_bounds(rect(0, 0, 0, 0), POINT { x: 0, y: 0 }, (10, 10)), Client::Unknown);
    assert_eq!(client_in_bounds(rect(0, 0, 100, 100), POINT { x: 0, y: 0 }, (0, 0)), Client::Unknown);
    assert_eq!(client_in_bounds(rect(0, 0, 100, 100), POINT { x: 200, y: 0 }, (10, 10)), Client::Unknown);
}

static PROBES: AtomicU32 = AtomicU32::new(0);
static FILLS_AFTER: AtomicU32 = AtomicU32::new(u32::MAX);

fn fake(_hwnd: usize) -> Client {
    let n = PROBES.fetch_add(1, Relaxed);
    if n >= FILLS_AFTER.load(Relaxed) {
        Client::Fills
    } else {
        Client::Inset(Rect { x: 0, y: 23, w: 2560, h: 1369 })
    }
}

#[test]
fn tracker_reprobes_on_size_change_or_timeout_and_keeps_the_canvas() {
    PROBES.store(0, Relaxed);
    FILLS_AFTER.store(3, Relaxed);
    let mut area = ClientArea::with_probe(7, fake);
    let t0 = Instant::now();
    let crop = Some(Rect { x: 0, y: 23, w: 2560, h: 1369 });
    assert_eq!(area.update((2560, 1392), t0), (crop, true));
    // Same size, within REPROBE: reused, no new measurement.
    assert_eq!(area.update((2560, 1392), t0 + Duration::from_millis(100)), (crop, true));
    assert_eq!(PROBES.load(Relaxed), 1);
    // A size change re-measures at once; the crop is clamped to the (smaller) frame.
    let (c, canvas) = area.update((2000, 1000), t0 + Duration::from_millis(150));
    assert_eq!((c, canvas), (Some(Rect { x: 0, y: 23, w: 2000, h: 977 }), true));
    assert_eq!(PROBES.load(Relaxed), 2);
    // REPROBE later: measured again.
    area.update((2000, 1000), t0 + Duration::from_millis(150) + REPROBE);
    assert_eq!(PROBES.load(Relaxed), 3);
    // The window went borderless: whole frame, still on the canvas (sticky).
    assert_eq!(area.update((2560, 1440), t0 + Duration::from_secs(2)), (None, true));
}

static WOBBLES: AtomicU32 = AtomicU32::new(0);

/// A borderless game flipping 2560x1440 <-> 2560x1438: the client rect moves before the
/// frame bounds on some measurements, so a 2 px "inset" shows up for one probe.
fn wobbling(_hwnd: usize) -> Client {
    match WOBBLES.fetch_add(1, Relaxed) % 2 {
        0 => Client::Fills,
        _ => Client::Inset(Rect { x: 0, y: 0, w: 2560, h: 1438 }),
    }
}

#[test]
fn a_two_pixel_client_wobble_never_changes_the_encoder_size() {
    // VALORANT opened at 2560x1438 on 10-25 and 10-31, so that is the size to keep.
    let mut area = ClientArea::with_probe(7, wobbling);
    let t0 = Instant::now();
    let mut target = None;
    for flip in 0..12u64 {
        let frame = if flip % 2 == 0 { (2560, 1438) } else { (2560, 1440) };
        let (crop, canvas) = area.update(frame, t0 + Duration::from_millis(flip * 700));
        let layout = Layout::recorded(frame, crop, canvas, target).expect("a full size frame");
        assert_eq!(layout.out, (2560, 1438), "flip {flip}: a new frame size reopens NVENC and empties the buffer");
        assert!(!canvas, "flip {flip}: a 2 px inset is not a title bar");
        target = Some(layout.out);
    }
}

#[test]
fn a_real_title_bar_still_moves_a_native_session_to_the_canvas() {
    fn caption(_hwnd: usize) -> Client {
        Client::Inset(Rect { x: 0, y: 9, w: 2560, h: 1431 })
    }
    let mut area = ClientArea::with_probe(7, caption);
    let (crop, canvas) = area.update((2560, 1440), Instant::now());
    assert_eq!((crop, canvas), (Some(Rect { x: 0, y: 9, w: 2560, h: 1431 }), true), "9 px is past the tolerance");
}

#[test]
fn monitor_captures_never_probe() {
    let mut area = ClientArea::with_probe(0, |_| panic!("monitor captures have no window"));
    assert_eq!(area.update((3840, 2160), Instant::now()), (None, false));
}
