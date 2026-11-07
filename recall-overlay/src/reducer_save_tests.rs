//! Saves in flight (O2 backstop, O6 one name, O7 close saves).

use super::*;
use super::save::STILL_SAVING;
use crate::notice::SavePhase;
use serde_json::json;

fn status(name: &str) -> Value {
    json!({"mod":"session","event":{"type":"status","state":"recording","target":{"name":name,"exe":"game.exe","pid":42}}})
}

fn saving(seconds: u64) -> Value {
    json!({"mod":"session","event":{"type":"saving","seconds":seconds}})
}

fn saved(path: &str, seconds: f64) -> Value {
    json!({"mod":"engine","event":{"type":"saved","path":path,"seconds":seconds}})
}

fn failed(reason: &str) -> Value {
    json!({"mod":"engine","event":{"type":"save_failed","reason":reason}})
}

fn notice(r: &Reducer) -> NoticeView {
    r.view().notice.clone().expect("a notice shows")
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

const VALORANT: &str = "E:\\c\\VALORANT__2026-10-30__21-14-03.mp4";

#[test]
fn a_save_with_no_outcome_says_save_not_confirmed() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    r.apply_envelope(&saving(30), now);
    r.tick(now + CONFIRM_FOR - ms(1));
    assert_eq!(notice(&r).save, Some(SavePhase::Saving));
    r.tick(now + CONFIRM_FOR);
    let n = notice(&r);
    assert_eq!((n.title.as_str(), n.save, n.tone), ("Save not confirmed", Some(SavePhase::Failed), Tone::Failure));
}

#[test]
fn the_controllers_save_failed_ends_the_ring_and_nothing_follows() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    r.apply_envelope(&saving(30), now);
    r.apply_envelope(&failed("The recorder stopped during the save"), now + ms(500));
    assert_eq!((notice(&r).title.as_str(), notice(&r).save), ("Save failed", Some(SavePhase::Failed)));
    r.tick(now + CONFIRM_FOR + ms(1));
    assert!(!r.view().visible, "the failed save is not reported again as unconfirmed");
}

#[test]
fn saved_keeps_the_name_saving_showed() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("Counter-Strike 2"), now);
    r.apply_envelope(&saving(30), now);
    let before = notice(&r).detail;
    r.apply_envelope(&saved("E:\\c\\Counter-Strike-2__2026-11-07__10-00-00.mp4", 30.0), now + ms(900));
    assert_eq!(notice(&r).title, "Clip saved");
    assert_eq!(notice(&r).detail, before, "only the title word changes");
}

#[test]
fn a_long_title_keeps_the_clip_length() {
    let now = Instant::now();
    let mut r = Reducer::new();
    let name = "Tom Clancy's Rainbow Six Siege Deluxe Edition";
    assert_eq!(name.chars().count(), 45);
    r.apply_envelope(&status(name), now);
    r.apply_envelope(&saving(30), now);
    let detail = notice(&r).detail;
    assert!(detail.ends_with("· 0:30") && detail.chars().count() <= 40, "{detail:?}");
    r.apply_envelope(&saved("E:\\c\\Tom-Clancy-s__x.mp4", 30.0), now + ms(900));
    assert!(notice(&r).detail.ends_with("· 0:30"));
}

#[test]
fn two_saves_close_together_show_two_outcomes_and_the_second_ring() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    r.apply_envelope(&saving(30), now);
    r.apply_envelope(&saving(30), now + ms(500));
    assert!(r.apply_envelope(&saved(VALORANT, 30.0), now + ms(2_000)));
    let first = notice(&r);
    assert_eq!((first.title.as_str(), first.save), ("Clip saved", Some(SavePhase::Saved)));
    assert!(r.apply_envelope(&saved(VALORANT, 30.0), now + ms(2_500)), "the second outcome is a visible change");
    let second = notice(&r);
    assert_eq!(second.title, "2 clips saved");
    assert_ne!(first, second);
}

#[test]
fn after_an_outcome_a_save_still_in_flight_gets_its_ring_back() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    r.apply_envelope(&saving(30), now);
    r.apply_envelope(&saving(60), now + ms(500));
    r.apply_envelope(&saved(VALORANT, 30.0), now + ms(2_000));
    assert_eq!(notice(&r).save, Some(SavePhase::Saved));
    r.tick(now + ms(2_000) + SAVED_FOR);
    let ring = notice(&r);
    assert_eq!((ring.title.as_str(), ring.detail.as_str(), ring.save), ("Saving", "VALORANT · 1:00", Some(SavePhase::Saving)));
    r.apply_envelope(&saved(VALORANT, 60.0), now + ms(9_000));
    assert_eq!(notice(&r).title, "Clip saved", "the notice had ended: counts from one");
}

#[test]
fn a_mic_toast_during_a_save_hands_back_to_the_ring() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    r.apply_envelope(&saving(30), now);
    r.apply_envelope(&json!({"mod":"engine","event":{"type":"mic_muted","muted":true}}), now + ms(300));
    assert_eq!(notice(&r).title, "Microphone muted");
    r.tick(now + ms(300) + MIC_FOR);
    assert_eq!(notice(&r).save, Some(SavePhase::Saving));
}

#[test]
fn the_detail_never_passes_forty_characters() {
    assert_eq!(save::save_detail("VALORANT", 30), "VALORANT · 0:30");
    assert_eq!(save::save_detail("VALORANT", 0), "VALORANT");
    let long = save::save_detail(&"x".repeat(60), 125);
    assert_eq!((long.chars().count(), long.ends_with("… · 2:05")), (40, true), "{long}");
}

fn refused(reason: &str) -> Value {
    json!({"mod":"engine","event":{"type":"save_failed","reason":reason,"refused":true}})
}

fn rejected(reason: &str) -> Value {
    json!({"mod":"engine","event":{"type":"save_failed","reason":reason,"rejected":true}})
}

#[test]
fn a_queued_save_waits_from_when_the_one_ahead_answers() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    r.apply_envelope(&saving(30), now);
    r.apply_envelope(&saving(30), now);
    r.apply_envelope(&saved(VALORANT, 30.0), now + Duration::from_secs(25));
    r.tick(now + Duration::from_secs(35));
    let n = notice(&r);
    assert_eq!((n.title.as_str(), n.save), ("Saving", Some(SavePhase::Saving)), "the second save's wait began at 25 s");
}

#[test]
fn a_slow_save_says_still_saving_then_not_confirmed_at_two_minutes() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    r.apply_envelope(&saving(30), now);
    assert!(r.tick(now + SAVING_FOR), "a visible change at 30 s");
    let n = notice(&r);
    assert_eq!((n.title.as_str(), n.save, n.tone), (STILL_SAVING, Some(SavePhase::Saving), Tone::Neutral));
    assert_eq!(n.detail, "VALORANT · 0:30");
    r.tick(now + CONFIRM_FOR - ms(1));
    assert_eq!(notice(&r).title, STILL_SAVING);
    r.tick(now + CONFIRM_FOR);
    assert_eq!(notice(&r).title, "Save not confirmed");
}

#[test]
fn a_slow_save_that_lands_says_clip_saved_and_nothing_else() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    r.apply_envelope(&saving(600), now);
    r.tick(now + Duration::from_secs(70));
    assert_eq!(notice(&r).title, STILL_SAVING);
    r.apply_envelope(&saved(VALORANT, 600.0), now + Duration::from_secs(80));
    assert_eq!(notice(&r).title, "Clip saved");
    r.tick(now + CONFIRM_FOR + ms(1));
    assert!(!r.view().visible, "no late failure for the landed save");
}

#[test]
fn a_refused_save_takes_no_saves_slot() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("Counter-Strike 2"), now);
    r.apply_envelope(&saving(30), now);
    let ring = notice(&r).detail;
    r.apply_envelope(&refused("Recorder isn't responding"), now + ms(300));
    assert_eq!(notice(&r).title, "Save failed");
    r.tick(now + ms(300) + FAILURE_FOR);
    assert_eq!(notice(&r).save, Some(SavePhase::Saving), "the real save's ring comes back");
    r.apply_envelope(&saved(r"E:\c\Counter-Strike-2__x.mp4", 30.0), now + ms(6_000));
    assert_eq!(notice(&r).detail, ring, "the real save keeps the name Saving showed");
}

#[test]
fn an_engine_rejection_fails_the_newest_save() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("Alpha"), now);
    r.apply_envelope(&saving(30), now);
    r.apply_envelope(&status("Beta"), now + ms(400));
    r.apply_envelope(&saving(30), now + ms(500));
    r.apply_envelope(&rejected("previous save still writing"), now + ms(600));
    r.apply_envelope(&saved(r"E:\c\x__y.mp4", 30.0), now + ms(5_000));
    assert_eq!(notice(&r).detail, "Alpha · 0:30", "the oldest save is the one that landed");
    r.tick(now + ms(5_000) + SAVED_FOR);
    assert!(!r.view().visible || notice(&r).save != Some(SavePhase::Saving), "nothing is left in flight");
}

#[test]
fn unanswered_saves_end_together_in_one_notice() {
    let now = Instant::now();
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    for i in 0..3 {
        r.apply_envelope(&saving(30), now + ms(i * 400));
    }
    r.tick(now + CONFIRM_FOR);
    let n = notice(&r);
    assert_eq!((n.title.as_str(), n.save), ("3 saves not confirmed", Some(SavePhase::Failed)), "one window for the queue");
    r.tick(now + CONFIRM_FOR + Duration::from_secs(5) + FAILURE_FOR);
    assert!(!r.view().visible || notice(&r).save != Some(SavePhase::Saving), "no ring comes back for 2 more minutes");
}

#[test]
fn queued_saves_that_keep_answering_keep_their_ring() {
    let now = Instant::now();
    let s = Duration::from_secs;
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    for _ in 0..3 {
        r.apply_envelope(&saving(30), now);
    }
    r.apply_envelope(&saved(VALORANT, 30.0), now + s(90));
    r.apply_envelope(&saved(VALORANT, 30.0), now + s(180));
    r.tick(now + s(200));
    assert_eq!(notice(&r).save, Some(SavePhase::Saving), "the third save is still being written");
    r.tick(now + s(299));
    assert_eq!(notice(&r).title, STILL_SAVING, "the engine answered 119 s ago");
    r.apply_envelope(&saved(VALORANT, 30.0), now + s(299));
    assert_eq!(notice(&r).title, "Clip saved", "it lands");
}

fn engine_writing(bytes: Option<u64>) -> Value {
    let mut event = json!({"type":"status","source":"VALORANT","fps":60,"buffer_secs":1800.0,"encoder":"h264_nvenc","encoder_error":null});
    if let Some(bytes) = bytes {
        event["save_writing"] = json!({"bytes": bytes, "queued": 0});
    }
    json!({"mod":"engine","event":event})
}

#[test]
fn a_save_whose_bytes_keep_growing_keeps_still_saving_past_120_s() {
    let now = Instant::now();
    let s = Duration::from_secs;
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    r.apply_envelope(&saving(1800), now);
    for t in 1..=265u64 {
        r.apply_envelope(&engine_writing(Some(t * 42_000_000)), now + s(t));
        r.tick(now + s(t) + ms(500));
        let n = notice(&r);
        assert_eq!(n.save, Some(SavePhase::Saving), "{t} s: the ring holds while the bytes grow");
        if t >= SAVING_FOR.as_secs() {
            assert_eq!(n.title, STILL_SAVING, "{t} s");
        }
    }
    r.apply_envelope(&saved(VALORANT, 1800.0), now + s(266));
    assert_eq!(notice(&r).title, "Clip saved");
}

#[test]
fn a_save_whose_bytes_stop_growing_is_unconfirmed_120_s_later() {
    let now = Instant::now();
    let s = Duration::from_secs;
    let mut r = Reducer::new();
    r.apply_envelope(&status("VALORANT"), now);
    r.apply_envelope(&saving(1800), now);
    r.apply_envelope(&engine_writing(Some(1_000)), now + s(50));
    for t in 51..=169u64 {
        r.apply_envelope(&engine_writing(Some(1_000)), now + s(t)); // stuck
        r.tick(now + s(t) + ms(1));
    }
    r.tick(now + s(169) + ms(999));
    assert_eq!(notice(&r).save, Some(SavePhase::Saving), "120 s since the last growth not yet up");
    r.tick(now + s(170));
    assert_eq!(notice(&r).title, "Save not confirmed");
}
