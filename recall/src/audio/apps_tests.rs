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
