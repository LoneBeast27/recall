//! Each user action against an engine whose stdin queue is full (a wedged engine): the line
//! is not queued, the caller says "Recorder isn't responding", and nothing is marked in
//! flight or cued. Gate P1 on d1e057e: these were all silent.

use super::*;
use crate::supervisor::TestPipe;

#[test]
fn a_save_to_a_wedged_engine_says_so_and_neither_ticks_nor_waits_for_it() {
    let full = TestPipe::wedged_and_full();
    let queued = full.send(r#"{"cmd":"save_clip","seconds":120}"#);
    assert_eq!(save_effects(queued), [Effect::NotResponding], "a wedged engine played the 'saved' tick and stayed busy");
    let ok = TestPipe::healthy();
    let queued = ok.send(r#"{"cmd":"save_clip","seconds":120}"#);
    assert_eq!(save_effects(queued), [Effect::MarkPending, Effect::Announce, Effect::Cue], "queued first, then marked and cued");
}

#[test]
fn bookmark_screenshot_and_record_to_a_wedged_engine_say_so() {
    let full = TestPipe::wedged_and_full();
    for cmd in ["bookmark", "screenshot", "record_start", "record_stop", "record_toggle"] {
        let queued = full.send(&format!(r#"{{"cmd":"{cmd}"}}"#));
        assert_eq!(command_effects(queued), [Effect::NotResponding], "{cmd}");
    }
    assert_eq!(command_effects(TestPipe::healthy().send(r#"{"cmd":"bookmark"}"#)), [Effect::MarkPending, Effect::Announce]);
}

#[test]
fn a_mic_mute_to_a_wedged_engine_says_so() {
    let full = TestPipe::wedged_and_full();
    assert_eq!(mic_effects(full.send(r#"{"cmd":"set_mic_mute","muted":true}"#)), [Effect::NotResponding]);
    assert_eq!(mic_effects(TestPipe::healthy().send(r#"{"cmd":"set_mic_mute","muted":true}"#)), []);
}

#[test]
fn the_hud_probe_is_no_user_action_so_a_wedged_engine_is_only_logged() {
    let full = TestPipe::wedged_and_full();
    assert_eq!(hud_probe_effects(full.send(r#"{"cmd":"hud_probe"}"#)), [Effect::LogOnly]);
    assert_eq!(hud_probe_effects(TestPipe::healthy().send(r#"{"cmd":"hud_probe"}"#)), []);
}

#[test]
fn the_legacy_app_save_to_a_wedged_engine_says_so() {
    let full = TestPipe::wedged_and_full();
    assert_eq!(legacy_save_effects(full.send(r#"{"cmd":"save_clip"}"#)), [Effect::NotResponding]);
    assert_eq!(legacy_save_effects(TestPipe::healthy().send(r#"{"cmd":"save_clip"}"#)), []);
}

#[test]
fn a_pause_whose_record_stop_cannot_be_queued_says_so_and_pauses_at_once() {
    // Waiting PAUSE_STOP_WAIT for a stop the engine never got gains nothing: the pause
    // applies now (the engine stops; the recording's .part is recovered on the next start).
    let full = TestPipe::wedged_and_full();
    assert_eq!(pause_stop_effects(full.send(r#"{"cmd":"record_stop"}"#)), [Effect::NotResponding, Effect::Proceed]);
    assert_eq!(pause_stop_effects(TestPipe::healthy().send(r#"{"cmd":"record_stop"}"#)), [Effect::MarkPending], "queued: wait for the stop");
}

#[test]
fn pipe_set_mode_and_audio_source_changes_to_a_wedged_engine_say_so_and_reply_an_error() {
    let full = TestPipe::wedged_and_full();
    for line in [r#"{"cmd":"set_mode","value":"hdr"}"#, r#"{"cmd":"set_source","id":"mic","gain_db":-3.0}"#] {
        assert_eq!(pipe_setting_effects(full.send(line)), [Effect::NotResponding, Effect::ReplyError], "{line}");
        assert_eq!(pipe_setting_effects(TestPipe::healthy().send(line)), [], "{line}");
    }
}

#[test]
fn automatic_lines_to_a_wedged_engine_are_logged() {
    // No user asked: the retarget's and sleep's record_stop, the fresh-engine intents.
    let full = TestPipe::wedged_and_full();
    assert_eq!(automatic_effects(full.send(r#"{"cmd":"record_stop"}"#)), [Effect::LogOnly]);
    assert_eq!(automatic_effects(TestPipe::healthy().send(r#"{"cmd":"record_stop"}"#)), []);
}

/// The steps a save takes, in order.
#[derive(Default)]
struct Steps {
    queued: bool,
    log: Vec<&'static str>,
}

impl SavePort for Steps {
    fn queue(&mut self, _line: &str) -> bool {
        self.log.push("queue");
        self.queued
    }
    fn mark_pending(&mut self) {
        self.log.push("mark_pending");
    }
    fn announce(&mut self, _seconds: u32) {
        self.log.push("announce");
    }
    fn cue(&mut self) {
        self.log.push("cue");
    }
    fn not_responding(&mut self) {
        self.log.push("not_responding");
    }
}

#[test]
fn the_controller_save_queues_first_then_marks_pending_then_ticks() {
    // `save` is what `Controller::save_clip` runs (through `ControllerSave`): the order is
    // the live path's, not a copy's.
    let mut ok = Steps { queued: true, ..Steps::default() };
    assert!(save(&mut ok, 30, false));
    assert_eq!(ok.log, ["queue", "mark_pending", "announce", "cue"]);
    let mut wedged = Steps::default();
    assert!(!save(&mut wedged, 30, false));
    assert_eq!(wedged.log, ["queue", "not_responding"], "no pending save, no tick for a wedged engine");
    let drain: serde_json::Value = serde_json::from_str(&save_line(120, true)).unwrap();
    assert_eq!(drain, serde_json::json!({"cmd":"save_clip","seconds":120,"at_last_frame":true}), "a drain save ends at the last frame");
}
