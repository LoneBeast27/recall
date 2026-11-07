//! Saves in flight (O2 backstop, O6 one name, O7 close saves).

use super::*;
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
    r.tick(now + SAVING_FOR - ms(1));
    assert_eq!(notice(&r).save, Some(SavePhase::Saving));
    r.tick(now + SAVING_FOR);
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
    r.tick(now + SAVING_FOR + ms(1));
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
