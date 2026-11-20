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
    // The canvas is the client area, even: 2560x1369 -> 2560x1368.
    let canvas = Some((2560, 1368));
    assert_eq!(area.update((2560, 1392), t0), (crop, canvas));
    // Same size, within REPROBE: reused, no new measurement.
    assert_eq!(area.update((2560, 1392), t0 + Duration::from_millis(100)), (crop, canvas));
    assert_eq!(PROBES.load(Relaxed), 1);
    // A size change re-measures at once; the crop is clamped to the (smaller) frame.
    let resized = area.update((2000, 1000), t0 + Duration::from_millis(150));
    assert_eq!(resized, (Some(Rect { x: 0, y: 23, w: 2000, h: 977 }), canvas), "the canvas keeps its size");
    assert_eq!(PROBES.load(Relaxed), 2);
    // REPROBE later: measured again.
    area.update((2000, 1000), t0 + Duration::from_millis(150) + REPROBE);
    assert_eq!(PROBES.load(Relaxed), 3);
    // The window went borderless: whole frame, still on the canvas (sticky).
    assert_eq!(area.update((2560, 1440), t0 + Duration::from_secs(2)), (None, canvas));
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
        assert!(canvas.is_none(), "flip {flip}: a 2 px inset is not a title bar");
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
    assert_eq!((crop, canvas), (Some(Rect { x: 0, y: 9, w: 2560, h: 1431 }), Some((2560, 1430))), "9 px is past the tolerance");
}

static SWITCHES: AtomicU32 = AtomicU32::new(0);

/// Borderless for the first probe, then a title bar (the game switched to windowed).
fn goes_windowed(_hwnd: usize) -> Client {
    match SWITCHES.fetch_add(1, Relaxed) {
        0 => Client::Fills,
        _ => Client::Inset(Rect { x: 1, y: 31, w: 1600, h: 900 }),
    }
}

#[test]
fn a_native_session_that_gains_a_title_bar_keeps_its_encoder_size() {
    let mut area = ClientArea::with_probe(7, goes_windowed);
    let t0 = Instant::now();
    assert_eq!(area.update((2560, 1440), t0), (None, None));
    let (crop, canvas) = area.update((1602, 932), t0 + Duration::from_millis(16));
    assert_eq!(crop, Some(Rect { x: 1, y: 31, w: 1600, h: 900 }));
    assert_eq!(canvas, Some((2560, 1440)), "a new size would reopen NVENC and empty the buffer");
}

#[test]
fn windowed_games_record_at_their_own_size_and_shape() {
    // 16:9 below 1440p, and 16:10 laptop panels: no bars on any side.
    static CLIENT: std::sync::Mutex<(u32, u32)> = std::sync::Mutex::new((0, 0));
    fn sized(_hwnd: usize) -> Client {
        let (w, h) = *CLIENT.lock().unwrap();
        Client::Inset(Rect { x: 1, y: 31, w, h })
    }
    for client in [(1600, 900), (1920, 1200), (2560, 1600), (1280, 720), (1366, 768)] {
        *CLIENT.lock().unwrap() = client;
        let mut area = ClientArea::with_probe(7, sized);
        let frame = (client.0 + 2, client.1 + 32);
        let (crop, canvas) = area.update(frame, Instant::now());
        let layout = Layout::recorded(frame, crop, canvas, None).expect("encodable");
        assert_eq!(layout.out, client, "{client:?}");
        assert_eq!(layout.dst, Rect::full(client), "{client:?}: the picture fills the frame");
        assert_eq!(layout.src.size(), layout.dst.size(), "{client:?}: pixel-exact, not scaled");
    }
}

#[test]
fn monitor_captures_never_probe() {
    let mut area = ClientArea::with_probe(0, |_| panic!("monitor captures have no window"));
    assert_eq!(area.update((3840, 2160), Instant::now()), (None, None));
}

/// A scripted window: each test sets what the next probes return.
static SCRIPT: std::sync::Mutex<Client> = std::sync::Mutex::new(Client::Unknown);

fn scripted(_hwnd: usize) -> Client {
    *SCRIPT.lock().unwrap()
}

/// Runs `steps` (frame size, what the window measures as, seconds since the first frame)
/// through one tracker; returns each step's recorded size.
fn record(steps: &[((u32, u32), Client, f64)]) -> Vec<(u32, u32)> {
    record_on((2560, 1440), steps)
}

/// [`record`] on a monitor of `monitor` size.
fn record_on(monitor: (u32, u32), steps: &[((u32, u32), Client, f64)]) -> Vec<(u32, u32)> {
    let _one_at_a_time = SCRIPT_RUN.lock().unwrap_or_else(|e| e.into_inner());
    *MONITOR.lock().unwrap() = monitor;
    let mut area = ClientArea::with_probe(7, scripted).with_monitor(|_| Some(*MONITOR.lock().unwrap()));
    let t0 = Instant::now();
    let mut target = None;
    steps
        .iter()
        .map(|&(frame, client, at)| {
            *SCRIPT.lock().unwrap() = client;
            let (crop, canvas) = area.update(frame, t0 + Duration::from_secs_f64(at));
            let layout = Layout::recorded_at(frame, crop, canvas, target, None).expect("encodable");
            target = Some(layout.out);
            layout.out
        })
        .collect()
}

static SCRIPT_RUN: std::sync::Mutex<()> = std::sync::Mutex::new(());
static MONITOR: std::sync::Mutex<(u32, u32)> = std::sync::Mutex::new((2560, 1440));

fn titled(w: u32, h: u32) -> Client {
    Client::Inset(Rect { x: 8, y: 31, w, h })
}

#[test]
fn a_small_start_window_then_fullscreen_records_at_full_size() {
    // Observed: a 1280x720 window first, then 2560x1440 fullscreen on the same window.
    let out = record(&[
        ((1296, 759), titled(1280, 720), 0.0),
        ((2560, 1440), Client::Fills, 3.0),
        ((2560, 1440), Client::Fills, 3.016),
        ((2560, 1440), Client::Fills, 3.033),
        ((2560, 1440), Client::Fills, 60.0),
    ]);
    assert_eq!(out[0], (1280, 720), "the start window records at its own size");
    assert_eq!(out[1], (1280, 720), "first fullscreen frame: fitted, no reopen yet");
    assert_eq!(out[2], (2560, 1440), "second frame at that size: full size (was 1280x720 all session)");
    assert_eq!(out[4], (2560, 1440));
}

#[test]
fn a_small_splash_window_never_sets_the_clip_size() {
    // 854x480 titled splash, then a titled 1920x1080 game window.
    let out = record(&[
        ((870, 519), titled(854, 480), 0.0),
        ((1936, 1119), titled(1920, 1080), 5.0),
        ((1936, 1119), titled(1920, 1080), 6.2),
    ]);
    assert_eq!(out, [(854, 480), (854, 480), (1920, 1080)]);
}

#[test]
fn a_borderless_splash_then_a_titled_window_records_the_window() {
    // 640x480 borderless splash seeds the canvas from the size it recorded at; the bigger
    // titled window then moves it.
    let out = record(&[
        ((640, 480), Client::Fills, 0.0),
        ((1936, 1119), titled(1920, 1080), 2.0),
        ((1936, 1119), titled(1920, 1080), 3.1),
    ]);
    assert_eq!(out, [(640, 480), (640, 480), (1920, 1080)]);
}

#[test]
fn a_window_being_dragged_never_reopens_the_encoder() {
    // A new size every 100 ms for 3 s: nothing holds for SETTLE, so the canvas stays.
    let mut steps = vec![((1296, 759), titled(1280, 720), 0.0)];
    for i in 1..30u32 {
        let (w, h) = (1280 + i * 20, 720 + i * 10);
        steps.push(((w + 16, h + 39), titled(w, h), f64::from(i) * 0.1));
    }
    assert!(record(&steps).iter().all(|&s| s == (1280, 720)));
}

#[test]
fn a_switch_to_fullscreen_moves_the_clip_once_it_holds() {
    let out = record(&[
        ((1296, 759), titled(1280, 720), 0.0),
        ((2560, 1440), Client::Fills, 20.0),
        ((2560, 1440), Client::Fills, 20.016),
    ]);
    assert_eq!(out, [(1280, 720), (1280, 720), (2560, 1440)], "two frames at the new size");
}

#[test]
fn the_clip_follows_until_the_game_first_fills_its_monitor() {
    // A windowed game, F11 two minutes in: its first fullscreen moves the clip (no clock), and
    // after that a window again (alt-enter) is fitted into the clip, nothing reopened.
    let out = record(&[
        ((1296, 759), titled(1280, 720), 0.0),
        ((2560, 1440), Client::Fills, 120.0),
        ((2560, 1440), Client::Fills, 120.016),
        ((1936, 1119), titled(1920, 1080), 130.0),
        ((1936, 1119), titled(1920, 1080), 135.0),
    ]);
    assert_eq!(out, [(1280, 720), (1280, 720), (2560, 1440), (2560, 1440), (2560, 1440)]);
}

#[test]
fn once_the_game_has_filled_its_monitor_a_bigger_window_stays_fitted() {
    // Fullscreen on a 1080p monitor, then a 1440p window (moved to another monitor): settled,
    // so the clip keeps its size instead of reopening.
    let out = record_on((1920, 1080), &[
        ((1296, 759), titled(1280, 720), 0.0),
        ((1920, 1080), Client::Fills, 2.0),
        ((1920, 1080), Client::Fills, 2.016),
        ((2576, 1479), titled(2560, 1440), 20.0),
        ((2576, 1479), titled(2560, 1440), 25.0),
    ]);
    assert_eq!(out, [(1280, 720), (1280, 720), (1920, 1080), (1920, 1080), (1920, 1080)]);
}

#[test]
fn a_borderless_splash_smaller_than_the_monitor_does_not_settle_the_clip() {
    // A 640x480 borderless splash fills its own frame but not the monitor: the game window
    // that follows still moves the clip.
    let out = record(&[
        ((640, 480), Client::Fills, 0.0),
        ((640, 480), Client::Fills, 0.5),
        ((1936, 1119), titled(1920, 1080), 2.0),
        ((1936, 1119), titled(1920, 1080), 3.1),
    ]);
    assert_eq!(out, [(640, 480), (640, 480), (640, 480), (1920, 1080)]);
}

#[test]
fn an_accidental_flip_back_and_forth_reopens_at_most_once_per_gap() {
    // Windowed -> fullscreen -> windowed bigger -> fullscreen, each held 1.2 s: the first
    // move lands; the next waits until MOVE_GAP after it.
    let mut steps = vec![((1296, 759), titled(1280, 720), 0.0)];
    let flips = [((1936, 1119), titled(1920, 1080)), ((2576, 1479), titled(2560, 1440))];
    let mut t = 0.0;
    for (frame, client) in flips {
        for _ in 0..3 {
            t += 0.6;
            steps.push((frame, client, t));
        }
    }
    let out = record(&steps);
    assert_eq!(out[3], (1920, 1080), "first settled move");
    assert!(out[4..].iter().all(|&s| s == (1920, 1080)), "a second move inside the gap waits: {out:?}");
    let late = record(&[
        ((1296, 759), titled(1280, 720), 0.0),
        ((1936, 1119), titled(1920, 1080), 1.0),
        ((1936, 1119), titled(1920, 1080), 2.1),
        ((2576, 1479), titled(2560, 1440), 5.0),
        ((2576, 1479), titled(2560, 1440), 12.2),
    ]);
    assert_eq!(late, [(1280, 720), (1280, 720), (1920, 1080), (1920, 1080), (2560, 1440)], "past the gap it moves");
}

#[test]
fn an_accidental_desktop_resolution_drop_keeps_the_clip_size() {
    // Borderless 2560x1440 on a titled-start canvas, then the desktop drops to 1920x1080:
    // smaller, so it is fitted into the clip and nothing reopens.
    let out = record(&[
        ((2576, 1479), titled(2560, 1440), 0.0),
        ((1920, 1080), Client::Fills, 30.0),
        ((1920, 1080), Client::Fills, 31.5),
    ]);
    assert_eq!(out, [(2560, 1440); 3], "a smaller picture is fitted, never a reopen");
}

#[test]
fn a_smaller_window_keeps_the_canvas() {
    let out = record(&[
        ((1936, 1119), titled(1920, 1080), 0.0),
        ((1296, 759), titled(1280, 720), 2.0),
        ((1296, 759), titled(1280, 720), 4.0),
    ]);
    assert_eq!(out, [(1920, 1080); 3], "a shrink is centred on the canvas, never a reopen");
}

#[test]
fn a_fill_that_sends_one_frame_moves_after_a_quarter_second() {
    // A mode change whose frames come slowly (a static menu): one frame, then the next 300 ms on.
    let out = record(&[
        ((1296, 759), titled(1280, 720), 0.0),
        ((2560, 1440), Client::Fills, 5.0),
        ((2560, 1440), Client::Fills, 5.3),
    ]);
    assert_eq!(out, [(1280, 720), (1280, 720), (2560, 1440)]);
}

#[test]
fn a_fill_still_changing_size_waits_for_one_size() {
    // A mode change that passes through a second size: each new size starts the count again.
    let out = record(&[
        ((1296, 759), titled(1280, 720), 0.0),
        ((1920, 1080), Client::Fills, 5.0),
        ((2560, 1440), Client::Fills, 5.016),
        ((2560, 1440), Client::Fills, 5.033),
    ]);
    assert_eq!(out, [(1280, 720), (1280, 720), (1280, 720), (2560, 1440)]);
}
