use super::*;

/// 1 tick = 1 µs keeps the synthetic frame times exact.
const HZ: i64 = 1_000_000;

fn steady(window: &mut FrameWindow, start: i64, frame_us: i64, count: i64) -> i64 {
    for i in 0..count {
        window.push(start + i * frame_us);
    }
    start + (count - 1) * frame_us
}

#[test]
fn a_steady_125_fps_game_reads_125_and_a_matching_low() {
    let mut window = FrameWindow::new(HZ);
    let last = steady(&mut window, 10_000_000, 8_000, 1_000); // 8 ms frames, 8 s of them
    let stats = window.stats(last);
    assert_eq!(stats.fps, Some(125), "presents in (newest - 1 s, newest]");
    let low = stats.low1.expect("plenty of frames");
    assert!((low - 125.0).abs() < 1e-9, "every frame is 8 ms, so the 1% low is 125: {low}");
}

#[test]
fn the_one_percent_low_is_the_99th_percentile_frame_time_over_five_seconds() {
    let mut window = FrameWindow::new(HZ);
    // 5 s window at 100 FPS = 500 frame times; 10 of them (2%) are 25 ms hitches.
    let mut t = 0;
    for i in 0..600 {
        t += if i % 60 == 59 { 25_000 } else { 10_000 };
        window.push(t);
    }
    let stats = window.stats(t);
    assert_eq!(stats.low1.map(|l| l.round()), Some(40.0), "1000 / 25 ms");
    // 1 hitch in 500 frame times (0.2%) stays above the 99th percentile.
    let mut calm = FrameWindow::new(HZ);
    let mut t = 0;
    for i in 0..600 {
        t += if i == 550 { 25_000 } else { 10_000 };
        calm.push(t);
    }
    assert_eq!(calm.stats(t).low1.map(|l| l.round()), Some(100.0));
}

#[test]
fn nearest_rank_percentile_on_small_and_exact_sets() {
    let mut ten: Vec<f64> = (1..=10).map(f64::from).collect();
    assert_eq!(low_1_percent(&mut ten), Some(100.0), "rank ceil(9.9) = 10 -> the 10 ms frame");
    let mut nine = vec![5.0; 9];
    assert_eq!(low_1_percent(&mut nine), None, "under MIN_LOW_SAMPLES");
    let mut zeros = vec![0.0; 20];
    assert_eq!(low_1_percent(&mut zeros), None, "zero-length frames have no rate");
    let mut hundred: Vec<f64> = (1..=100).map(f64::from).collect();
    assert_eq!(low_1_percent(&mut hundred), Some(1000.0 / 99.0), "rank 99 of 100");
}

#[test]
fn frame_times_are_the_gaps_between_consecutive_presents() {
    assert_eq!(frame_times_ms(&[0, 16_000, 33_000, 50_000], HZ), vec![16.0, 17.0, 17.0]);
    assert!(frame_times_ms(&[7], HZ).is_empty());
    assert_eq!(frame_times_ms(&[0, 10_000_000], 10_000_000), vec![1000.0], "10 MHz QPC");
}

#[test]
fn out_of_order_delivery_is_sorted_in() {
    let mut window = FrameWindow::new(HZ);
    // Two CPUs' buffers: odd frames arrive first, then the even ones.
    for i in (1..200).step_by(2) {
        window.push(i * 10_000);
    }
    for i in (0..200).step_by(2) {
        window.push(i * 10_000);
    }
    let stats = window.stats(199 * 10_000);
    assert_eq!(stats.fps, Some(100));
    assert_eq!(stats.low1.map(|l| l.round()), Some(100.0), "no 20 ms gaps left after sorting");
}

#[test]
fn the_fps_window_ends_at_the_newest_delivered_present() {
    let mut window = FrameWindow::new(HZ);
    let last = steady(&mut window, 0, 10_000, 300);
    // ETW delivers up to ~1 s late: the wall clock is ahead, the count is not cut.
    assert_eq!(window.stats(last + 900_000).fps, Some(100));
    assert_eq!(window.stats(last + 2_900_000).fps, Some(100), "still within STALE_SECS");
    assert_eq!(window.stats(last + 3_100_000), FrameStats::default(), "stopped presenting: no numbers");
}

#[test]
fn old_presents_are_dropped_and_an_empty_window_has_no_numbers() {
    let mut window = FrameWindow::new(HZ);
    assert_eq!(window.stats(0), FrameStats::default());
    steady(&mut window, 0, 1_000, 20_000); // 20 s at 1000 FPS
    assert!(window.stamps.len() <= 6_001, "keeps ~6 s: {}", window.stamps.len());
    window.clear();
    assert_eq!(window.stats(20_000_000), FrameStats::default());
}

#[test]
fn a_slow_start_has_fps_before_it_has_a_low() {
    let mut window = FrameWindow::new(HZ);
    let last = steady(&mut window, 0, 100_000, 5); // 10 FPS, 5 presents
    let stats = window.stats(last);
    assert_eq!(stats.fps, Some(5), "only half a second of presents so far");
    assert_eq!(stats.low1, None, "4 frame times < MIN_LOW_SAMPLES");
}
