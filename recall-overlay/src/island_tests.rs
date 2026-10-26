use super::*;
use crate::notice::Tone;
use crate::reducer::{Reducer, NOTICE_FOR};
use serde_json::{json, Value};
use std::time::Duration;

fn status_with(notices: bool, stats: bool) -> Value {
    json!({"mod":"session","event":{"type":"status","state":"recording","overlay_stats":stats,"fps_source":"etw",
        "overlay":{"enabled":notices,"corner":"top-right","running":true,"stats_style":"minimal"},
        "target":{"name":"VALORANT","exe":"VALORANT-Win64-Shipping.exe","pid":42}}})
}

fn session_status(stats: bool) -> Value {
    status_with(true, stats)
}

fn game_notice() -> Value {
    json!({"mod":"session","event":{"type":"notice","kind":"game","title":"Recording","detail":"VALORANT"}})
}

fn engine_status() -> Value {
    json!({"mod":"engine","event":{"type":"status","source":"VALORANT","fps":144,"buffer_secs":120.0,"encoder":"h264_nvenc"}})
}

fn shown(reducer: &Reducer) -> Option<Content> {
    content(reducer.view(), reducer.session())
}

#[test]
fn hidden_by_default_and_stats_follow_the_toggle() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    assert_eq!(shown(&reducer), None);
    reducer.apply_envelope(&engine_status(), now);
    reducer.apply_envelope(&session_status(false), now);
    assert_eq!(shown(&reducer), None, "plain status never shows the island");
    assert!(reducer.apply_envelope(&session_status(true), now));
    assert_eq!(shown(&reducer), Some(Content::Stats));
    assert_eq!(reducer.buffer_secs(), 120);
    assert!(reducer.apply_envelope(&session_status(false), now));
    assert_eq!(shown(&reducer), None);
}

#[test]
fn a_game_notice_shows_for_three_seconds_then_hides() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&session_status(false), now);
    assert!(reducer.apply_envelope(&game_notice(), now));
    let Some(Content::Notice(notice)) = shown(&reducer) else { panic!("notice expected") };
    assert_eq!((notice.tone, notice.title.as_str(), notice.detail.as_str()), (Tone::Recording, "Recording", "VALORANT"));
    assert!(notice.game_font);
    assert!(!reducer.tick(now + NOTICE_FOR - Duration::from_millis(1)));
    assert!(shown(&reducer).is_some());
    assert!(reducer.tick(now + NOTICE_FOR));
    assert_eq!(shown(&reducer), None);
}

#[test]
fn a_notice_over_the_stats_returns_to_the_stats() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&engine_status(), now);
    reducer.apply_envelope(&session_status(true), now);
    reducer.apply_engine_event(&json!({"type":"saved","path":"V:\\c\\VALORANT__2026-10-26__10-00-00.mp4","seconds":120.0}), now);
    let Some(Content::Notice(notice)) = shown(&reducer) else { panic!("notice expected") };
    assert_eq!((notice.title.as_str(), notice.detail.as_str()), ("Clip saved", "VALORANT · 2:00"));
    reducer.apply_envelope(&engine_status(), now + Duration::from_secs(1));
    reducer.apply_envelope(&session_status(true), now + Duration::from_secs(1));
    assert!(matches!(shown(&reducer), Some(Content::Notice(_))), "status ticks keep the notice");
    reducer.tick(now + Duration::from_secs(3));
    assert_eq!(shown(&reducer), Some(Content::Stats));
}

#[test]
fn turning_stats_off_under_a_notice_hides_after_the_notice() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&session_status(true), now);
    reducer.apply_envelope(&game_notice(), now);
    reducer.apply_envelope(&session_status(false), now + Duration::from_secs(1));
    assert!(matches!(shown(&reducer), Some(Content::Notice(_))));
    reducer.tick(now + NOTICE_FOR);
    assert_eq!(shown(&reducer), None);
}

#[test]
fn disconnect_drops_the_stats_island() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&session_status(true), now);
    assert!(reducer.disconnected());
    assert_eq!(shown(&reducer), None);
    assert!(!reducer.session().overlay_stats);
}

#[test]
fn notices_follow_overlay_enabled_and_stats_follow_overlay_stats() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_engine_event(&json!({"type":"bookmarked"}), now);
    assert_eq!(shown(&reducer), None, "before the first status both are off");
    reducer.apply_envelope(&status_with(false, false), now);
    reducer.apply_envelope(&game_notice(), now);
    assert_eq!(shown(&reducer), None, "overlay.enabled = false: no notices");
    reducer.apply_envelope(&status_with(false, true), now);
    assert_eq!(shown(&reducer), Some(Content::Stats), "stats only, the notice stays hidden");
    reducer.apply_envelope(&status_with(true, true), now);
    reducer.apply_engine_event(&json!({"type":"mic_muted","muted":true}), now);
    assert!(matches!(shown(&reducer), Some(Content::Notice(_))));
}

#[test]
fn five_option_chord_hint_uses_two_rows() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&session_status(false), now);
    reducer.apply_envelope(&json!({"mod":"session","event":{"type":"chord","active":true,"leader":"Alt `","options":[
        {"key":"1","label":"Bookmark"},{"key":"2","label":"Save 30 s"},{"key":"3","label":"Screenshot"},
        {"key":"4","label":"Record"},{"key":"5","label":"Performance stats"}]}}), now);
    let Some(Content::Notice(notice)) = shown(&reducer) else { panic!("notice expected") };
    assert!(notice.chord);
    assert_eq!(notice.title, "1 Bookmark · 2 Save 30 s · 3 Screenshot");
    assert_eq!(notice.detail, "4 Record · 5 Performance stats");
}

#[test]
fn stats_content_reads_session_buffer_and_sample() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&engine_status(), now);
    reducer.apply_envelope(&session_status(true), now);
    let sample = StatsSnapshot { fps: Some(144), low1: Some(98), gpu: Some(71), gpu_temp: Some(64), cpu: Some(23), ..Default::default() };
    let st = stats_content(&reducer, &sample);
    assert!(st.recording && !st.paused);
    assert_eq!((st.replay_secs, st.fps, st.low1, st.fps_note), (120, Some(144), Some(98), None));
    let refused = StatsSnapshot { fps_source: FpsSource::NoPermission, ..Default::default() };
    assert_eq!(stats_content(&reducer, &refused).fps_note.as_deref(), Some("No frame timing"), "the overlay's own refusal");
    let waiting = StatsSnapshot { fps_source: FpsSource::Etw, ..Default::default() };
    assert_eq!(stats_content(&reducer, &waiting).fps_note, None, "no frames yet: a plain dash, no note");
    let mut paused = reducer;
    paused.apply_envelope(
        &json!({"mod":"session","event":{"type":"status","state":"paused","overlay_stats":true,"fps_source":"no_permission"}}),
        now,
    );
    let st = stats_content(&paused, &waiting);
    assert!(st.paused && !st.recording);
    assert_eq!(st.fps_note.as_deref(), Some("No frame timing"), "the controller's permission check");
    let measured = StatsSnapshot { fps: Some(60), ..waiting };
    assert_eq!(stats_content(&paused, &measured).fps_note, None, "a framerate never carries a note");
}

const W: f32 = 300.0;
const H: f32 = 44.0;

#[test]
fn the_island_grows_out_of_a_collapsed_pill_and_settles() {
    let now = Instant::now();
    let mut island = Island::new(now);
    assert!(!island.shown());
    island.update(Some((Content::Stats, W, H)), now);
    assert!(island.shown());
    assert_eq!(island.frame(now), Frame::collapsed(W, H));
    assert!(island.animating(now));
    let settled = now + DURATION;
    assert_eq!(island.frame(settled), Frame { w: W, h: H, alpha: 1.0 });
    assert!(!island.animating(settled));
}

#[test]
fn a_content_swap_resizes_without_collapsing_and_fades_the_new_content_in() {
    let now = Instant::now();
    let mut island = Island::new(now);
    island.update(Some((Content::Stats, W, H)), now);
    let later = now + Duration::from_secs(1);
    let notice = Content::Notice(NoticeView::new(Tone::Success, "Clip saved", "VALORANT · 2:00", true));
    island.update(Some((notice, 200.0, H)), later);
    assert_eq!(island.frame(later), Frame { w: W, h: H, alpha: 1.0 }, "starts from the stats size");
    assert_eq!(island.content_alpha(later), 0.0);
    assert_eq!(island.frame(later + DURATION).w, 200.0);
    assert_eq!(island.content_alpha(later + DURATION), 1.0);
    // Same content again (a stats value tick) neither fades nor restarts.
    island.update(Some((Content::Stats, W, H)), later + DURATION);
    island.update(Some((Content::Stats, W + 4.0, H)), later + DURATION * 2);
    assert!(island.content_alpha(later + DURATION * 3) >= 1.0);
}

#[test]
fn hiding_shrinks_then_reports_hidden_once() {
    let now = Instant::now();
    let mut island = Island::new(now);
    island.update(Some((Content::Stats, W, H)), now);
    let later = now + Duration::from_secs(1);
    island.update(None, later);
    assert!(island.shown(), "still on screen while shrinking");
    assert!(!island.tick(later + DURATION / 2));
    assert!(island.tick(later + DURATION));
    assert!(!island.shown() && island.content().is_none());
    assert!(!island.tick(later + DURATION * 2), "reported once");
}

#[test]
fn reshowing_mid_hide_continues_from_the_current_frame() {
    let now = Instant::now();
    let mut island = Island::new(now);
    island.update(Some((Content::Stats, W, H)), now);
    let hide = now + Duration::from_secs(1);
    island.update(None, hide);
    let mid = hide + DURATION / 2;
    let here = island.frame(mid);
    island.update(Some((Content::Stats, W, H)), mid);
    assert_eq!(island.frame(mid), here, "no jump back to collapsed");
    assert_eq!(island.frame(mid + DURATION), Frame { w: W, h: H, alpha: 1.0 });
}
