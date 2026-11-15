//! `next_deadline`: ticking only at the deadlines (and on each message) shows
//! the same thing, at the same moments, as the old 4 Hz tick.

use super::*;
use serde_json::json;

fn engine_status() -> Value {
    json!({"mod":"engine","event":{"type":"status","source":"Elden Ring","fps":117,"buffer_secs":59.6,"encoder":"h264_nvenc","encoder_error":null}})
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// One controller line at `at` ms.
fn timeline() -> Vec<(u64, Value)> {
    let mut lines = vec![(0, json!({"mod":"session","event":{"type":"status","state":"recording","target":{"name":"VALORANT","exe":"v.exe","pid":42}}}))];
    // A 1 s status keepalive until 10 s, then the feed stops (stale at 12.5 s).
    lines.extend((0..=10).map(|s| (s * 1_000, engine_status())));
    lines.push((1_000, json!({"mod":"session","event":{"type":"saving","seconds":30}})));
    lines.push((3_100, json!({"mod":"engine","event":{"type":"saved","path":"E:\\c\\VALORANT__x.mp4","seconds":30.0}})));
    lines.push((4_000, json!({"mod":"session","event":{"type":"saving","seconds":60}})));
    lines.push((5_300, json!({"mod":"engine","event":{"type":"mic_muted","muted":true}})));
    lines.push((6_700, json!({"mod":"session","event":{"type":"notice","title":"Paused","detail":"x"}})));
    lines.push((7_400, json!({"mod":"session","event":{"type":"chord","active":true,"leader":"Alt `","options":[{"key":"1","label":"Bookmark"}]}})));
    lines.sort_by_key(|(at, _)| *at);
    lines
}

#[test]
fn ticking_at_the_deadlines_matches_the_4_hz_tick() {
    let t0 = Instant::now();
    let (mut every, mut sleepy) = (Reducer::new(), Reducer::new());
    let lines = timeline();
    let mut sleepy_ticks = 0;
    // 126 s at 250 ms: past "Still saving…" (34 s) and "Save not confirmed" (124 s).
    for step in 0..=504u64 {
        let now = t0 + ms(step * 250);
        let due = sleepy.next_deadline(t0 + ms(step.saturating_sub(1) * 250));
        let mut message = false;
        for (_, line) in lines.iter().filter(|(at, _)| (step.saturating_sub(1) * 250 + 1..=step * 250).contains(at) || (step == 0 && *at == 0)) {
            every.apply_envelope(line, now);
            sleepy.apply_envelope(line, now);
            message = true;
        }
        every.tick(now);
        if message || due.is_some_and(|at| at <= now) {
            sleepy.tick(now);
            sleepy_ticks += 1;
        }
        assert_eq!(every.view(), sleepy.view(), "at {} ms", step * 250);
    }
    assert!(sleepy_ticks < 40, "a handful of wakes, not 505: {sleepy_ticks}");
    assert_eq!(sleepy.view().metrics, "Status feed stale");
    assert!(sleepy.view().primary.contains("not confirmed"), "{:?}", sleepy.view().primary);
}

#[test]
fn a_one_second_keepalive_never_goes_stale() {
    let t0 = Instant::now();
    let mut r = Reducer::new();
    assert_eq!(r.next_deadline(t0), None, "nothing pending, no timer");
    for s in 0..10 {
        let now = t0 + Duration::from_secs(s);
        r.apply_envelope(&engine_status(), now);
        let due = r.next_deadline(now).expect("the stale check");
        assert!(due > now + STALE_AFTER && due <= now + STALE_AFTER + ms(1));
        assert!(due > now + Duration::from_secs(1), "the next keepalive comes first");
        assert!(!r.tick(now + ms(999)));
    }
    let last = t0 + Duration::from_secs(9);
    let due = r.next_deadline(last).unwrap();
    assert!(!r.tick(due - ms(1)), "not stale before its deadline");
    assert!(r.tick(due), "stale at it");
    assert_eq!(r.next_deadline(due), None, "nothing more to wake for");
}

#[test]
fn a_notice_and_a_save_each_set_their_deadline() {
    let t0 = Instant::now();
    let mut r = Reducer::new();
    r.apply_engine_event(&json!({"type":"bookmarked"}), t0);
    assert_eq!(r.next_deadline(t0), Some(t0 + MARK_FOR));
    r.apply_envelope(&json!({"mod":"session","event":{"type":"saving","seconds":30}}), t0 + ms(100));
    // The ring replaced the bookmark: it holds until CONFIRM_FOR, turns slow at SAVING_FOR.
    assert_eq!(r.next_deadline(t0 + ms(100)), Some(t0 + ms(100) + SAVING_FOR));
    let slow = t0 + ms(100) + SAVING_FOR;
    assert!(r.tick(slow), "Still saving…");
    assert_eq!(r.next_deadline(slow), Some(t0 + ms(100) + CONFIRM_FOR));
}
