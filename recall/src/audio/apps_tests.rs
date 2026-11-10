//! App-track parsing, names and the leveller (no WASAPI, no processes).

use std::time::Duration;

use super::level::same_exe;
use super::{parse, AppSource, Leveller};

#[test]
fn the_list_parses_and_bad_entries_are_refused() {
    let apps = parse(r#"[{"exe":"player.exe","label":"Source A"},{"exe":"chat.exe"}]"#).unwrap();
    assert_eq!(apps.len(), 2);
    assert_eq!(apps[1].label, "");
    assert!(parse(r#"[{"exe":"C:\\apps\\a.exe"}]"#).is_err(), "no folders");
    assert!(parse(r#"[{"exe":""}]"#).is_err());
    let nine: Vec<String> = (0..9).map(|n| format!(r#"{{"exe":"a{n}.exe"}}"#)).collect();
    assert!(parse(&format!("[{}]", nine.join(","))).is_err(), "at most 8, like the sources list");
    assert!(parse("not json").is_err());
}

#[test]
fn a_track_is_named_by_its_label_else_the_exe_without_its_extension() {
    let app = |exe: &str, label: &str| AppSource { exe: exe.into(), label: label.into(), id: String::new(), gain_db: 0.0 };
    assert_eq!(app("player.exe", " Source A ").track_name(), "Source A");
    assert_eq!(app("player.EXE", "").track_name(), "player");
    assert_eq!(app("tool", "").track_name(), "tool");
    assert!(same_exe("Game.EXE", "game") && !same_exe("game.exe", "games.exe"));
}

const RATE: u32 = 48_000;
const PACKET: usize = 480; // 10 ms, mono

/// Feeds `seconds` of a never-silent tone through a leveller whose device clock runs at `speed`
/// times the wall clock; returns the written samples and the lag at the end in frames.
fn run_live(speed: f64, seconds: f64) -> (Vec<f32>, i64) {
    let mut level = Leveller::new(RATE);
    let mut out = Vec::new();
    let packets = (seconds * RATE as f64 * speed / PACKET as f64) as usize;
    for p in 0..packets {
        let samples: Vec<f32> = (0..PACKET).map(|i| 0.5 + 0.1 * (((p * PACKET + i) as f32) * 0.05).sin()).collect();
        // The device made this packet's last frame at (p + 1) * PACKET / (RATE * speed) seconds.
        let elapsed = Duration::from_secs_f64((p + 1) as f64 * PACKET as f64 / (RATE as f64 * speed));
        out.extend(level.live(elapsed, &samples, 1));
    }
    let due = (packets as f64 * PACKET as f64 / speed) as i64;
    (out.clone(), due - out.len() as i64)
}

fn longest_zero_run(samples: &[f32]) -> usize {
    samples.split(|s| *s != 0.0).map(<[f32]>::len).max().unwrap_or(0)
}

#[test]
fn a_slow_device_clock_is_corrected_without_silent_blocks() {
    // 2026-11-05 merge-gate review: silence padding by the wall clock (100 ms slack) put
    // 100 ms silent blocks into live sound when the device clock ran slow.
    let (out, lag) = run_live(0.9995, 120.0);
    assert!(longest_zero_run(&out) < RATE as usize / 100, "a silent run of 10 ms or more inside live sound");
    assert!(lag.unsigned_abs() <= (RATE as u64 * 25 / 1000), "still {lag} frames behind after 2 minutes");
}

#[test]
fn a_fast_device_clock_drops_single_frames_and_stays_level() {
    let (out, lag) = run_live(1.0005, 120.0);
    assert_eq!(longest_zero_run(&out), 0, "no silence at all inside live sound");
    assert!(lag.unsigned_abs() <= (RATE as u64 * 25 / 1000), "{lag} frames off after 2 minutes");
}

#[test]
fn correction_never_exceeds_a_tenth_of_a_percent() {
    let (out, _) = run_live(0.98, 10.0); // far beyond what may be corrected
    let fed = (10.0 * RATE as f64 * 0.98 / PACKET as f64) as usize * PACKET;
    assert!(out.len() as f64 <= fed as f64 * 1.0011, "{} frames out for {fed} in", out.len());
}

#[test]
fn silence_fills_only_while_no_stream_is_active_and_a_new_stream_starts_on_the_clock() {
    let mut level = Leveller::new(RATE);
    assert_eq!(level.idle(Duration::from_secs(1)), 48_000, "closed app: silence to the clock");
    assert_eq!(level.idle(Duration::from_millis(1_500)), 24_000);
    // The app starts playing at 2.0 s: 0.5 s of lead silence, then its first packet.
    let out = level.live(Duration::from_secs(2), &[0.5; PACKET], 1);
    assert_eq!(out.len(), 24_000 - PACKET + PACKET);
    assert_eq!(longest_zero_run(&out), 24_000 - PACKET, "the lead is silence, the packet is sound");
}

/// 2026-11-07 audit T1: Windows' clock runs on through sleep, so the first call after a long
/// sleep owed the whole sleep (8 h = 1.4 G frames per track) in one block.
#[test]
fn a_three_hour_clock_jump_owes_at_most_the_ring() {
    let cap = 60 * RATE as u64; // a 60 s ring
    let mut level = Leveller::new(RATE).with_cap(cap);
    assert_eq!(level.idle(Duration::from_secs(1)), 48_000);
    let asleep = Duration::from_secs(3 * 3600 + 1);
    assert!(level.idle(asleep) <= cap, "a 3 h jump owes more than the ring holds");
    // Re-based on the clock: the next 10 ms owe 10 ms.
    assert_eq!(level.idle(asleep + Duration::from_millis(10)), 480);
    // A stream that starts after the jump gets at most a ring of lead silence.
    let mut level = Leveller::new(RATE).with_cap(cap);
    let out = level.live(asleep, &[0.5; PACKET], 1);
    assert!(out.len() as u64 <= cap + PACKET as u64, "lead of {} frames", out.len());
}

/// 2026-11-07 audit T1: a sleep inside live sound was crept up on at 0.1 % (a duplicated frame
/// per thousand, for days) instead of being re-based on the clock.
#[test]
fn a_clock_jump_inside_live_sound_re_bases_instead_of_creeping() {
    let cap = 60 * RATE as u64;
    let mut level = Leveller::new(RATE).with_cap(cap);
    let first = level.live(Duration::from_millis(10), &[0.5; PACKET], 1);
    assert_eq!(first.len(), PACKET);
    // The PC slept 3 h mid-stream: the first packet after it carries at most a ring of
    // silence, then the packets keep their length and stay level.
    let mut t = Duration::from_secs(3 * 3600) + Duration::from_millis(20);
    let resumed = level.live(t, &[0.5; PACKET], 1);
    assert_eq!(resumed.len() as u64, cap + PACKET as u64, "a ring of silence, then the packet");
    for _ in 0..100 {
        t += Duration::from_millis(10);
        let out = level.live(t, &[0.5; PACKET], 1);
        assert_eq!(out.len(), PACKET, "frames duplicated to creep up to a 3 h lag");
    }
    assert_eq!(level.idle(t), 0, "level with the clock after the jump");
}

/// Review 2026-11-07: a 30 s sleep (shorter than the ring) that a live stream survives must be
/// silenced up to the clock like an idle track, not crept up on at 0.1 % (about 8 h for 30 s).
#[test]
fn a_short_sleep_inside_live_sound_lines_up_with_idle_tracks() {
    let cap = 120 * RATE as u64;
    let (mut live, mut quiet) = (Leveller::new(RATE).with_cap(cap), Leveller::new(RATE).with_cap(cap));
    assert_eq!(live.live(Duration::from_millis(10), &[0.5; PACKET], 1).len(), PACKET);
    assert_eq!(quiet.idle(Duration::from_millis(10)), 480);
    let t = Duration::from_secs(30) + Duration::from_millis(20);
    let resumed = live.live(t, &[0.5; PACKET], 1);
    let silence = resumed.len() - PACKET;
    assert_eq!(silence as u64, 30 * RATE as u64, "30 s of silence, then the packet");
    assert!(resumed[..silence].iter().all(|&x| x == 0.0) && resumed[silence..].iter().all(|&x| x == 0.5));
    // Both tracks now end on the clock: no lag left to creep.
    assert_eq!(live.idle(t), 0);
    assert_eq!(quiet.idle(t), 30 * RATE as u64 + 480);
    // Drift-sized lags (under a second) are still corrected gently, never with a block.
    let (out, _) = run_live(1.0005, 60.0);
    assert!(longest_zero_run(&out) == 0);
}

/// Merge-gate P3: an app track's silence up to the clock allocates nothing once the ring is
/// warm (it allocated one block per idle tick).
#[test]
fn an_app_tracks_silence_allocates_nothing() {
    let ring = crate::audio::capture_loop::tests::ring();
    super::push_silence(&ring, ring.capacity_frames());
    let ((), bytes) = crate::test_alloc::bytes_in(|| (0..20).for_each(|_| super::push_silence(&ring, 100)));
    assert_eq!(bytes, 0);
}

/// B3 for app tracks (design-audio-mute P4): a removed or disabled app source is retired live
/// and its track stores silence from the next packet, through the app capture path.
#[test]
fn a_retired_app_track_stores_silence_at_once() {
    let ring = std::sync::Arc::new(crate::audio::TrackRing::for_test("app-retire", 2));
    ring.push_app_capture(&[0.5; 960]);
    assert!(ring.snapshot(0.01).iter().any(|s| *s != 0.0), "live before");
    crate::audio::sources::register_for_test("app-retire", &ring);
    assert!(crate::audio::sources::set_live("app-retire", crate::audio::sources::LiveChange { retire: true, ..Default::default() }));
    ring.push_app_capture(&[0.5; 960]);
    assert!(ring.snapshot(0.01).iter().all(|s| *s == 0.0), "retired: silence");
    ring.release_startup_hold(); // a sync after it
    ring.push_app_capture(&[0.5; 960]);
    assert!(ring.snapshot(0.01).iter().all(|s| *s == 0.0), "still silence");
    let apps = include_str!("apps.rs");
    assert!(apps.contains("ring.push_app_capture(&leveled);"), "the app capture loop stores through it");
    assert!(!apps.contains("ring.push_capture(&leveled, false)"));
}
