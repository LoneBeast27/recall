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
