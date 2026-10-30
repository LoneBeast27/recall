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
    content(reducer.view(), reducer.session(), true)
}

#[test]
fn stats_show_only_while_the_game_is_in_front_but_notices_always() {
    let now = Instant::now();
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&engine_status(), now);
    reducer.apply_envelope(&session_status(true), now);
    assert_eq!(content(reducer.view(), reducer.session(), false), None, "another app in front: no stats");
    assert_eq!(content(reducer.view(), reducer.session(), true), Some(Content::Stats));
    reducer.apply_engine_event(&json!({"type":"mic_muted","muted":true}), now);
    assert!(matches!(content(reducer.view(), reducer.session(), false), Some(Content::Notice(_))), "notices still flash");
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
fn the_chord_opens_the_hint_card_under_whatever_the_island_shows() {
    let now = Instant::now();
    let chord = json!({"mod":"session","event":{"type":"chord","active":true,"leader":"Alt `","options":[
        {"key":"1","label":"Bookmark"},{"key":"2","label":"Save 30 s"},{"key":"3","label":"Screenshot"},
        {"key":"4","label":"Record"},{"key":"5","label":"Performance stats"}]}});
    let mut reducer = Reducer::new();
    reducer.apply_envelope(&session_status(false), now);
    reducer.apply_envelope(&chord, now);
    assert_eq!(shown(&reducer), Some(Content::Leader("Alt `".into())), "stats off: the small leader pill");
    let hint = reducer.view().hint.clone().expect("the card's options");
    let keys: Vec<(&str, &str)> = hint.options.iter().map(|o| (o.key.as_str(), o.label.as_str())).collect();
    assert_eq!(keys, [("1", "Bookmark"), ("2", "Save 30 s"), ("3", "Screenshot"), ("4", "Record"), ("5", "Performance stats")]);
    assert!(reducer.view().notice.is_none(), "the hint is not a notice any more");
    reducer.apply_envelope(&session_status(true), now);
    assert_eq!(shown(&reducer), Some(Content::Stats), "stats on: the stats pill stays, the card hangs below");
    reducer.tick(now + crate::reducer::CHORD_FOR);
    assert!(reducer.view().hint.is_none(), "closes with the chord window");
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

/// Appearing from nothing: pop + unravel, then the content fades in.
const OPEN: Duration = Duration::from_millis(400);
const HIDE: Duration = Duration::from_millis(550);

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn the_stats_pop_in_as_a_circle_that_unravels_then_the_values_fade_in() {
    let now = Instant::now();
    let mut island = Island::new(now);
    assert!(!island.shown());
    island.update(Some((Content::Stats, W, H)), now);
    assert!(island.shown() && island.animating(now));
    assert_eq!(island.frame(now), Frame { w: W, h: H, alpha: 1.0 }, "the open size is known at once");
    let circle = island.look(now + ms(60), f32::INFINITY);
    assert_eq!((circle.box_w, circle.unroll), (H, 0.0), "a circle the pill's height at the anchored end");
    assert!(circle.pill[2] == circle.pill[3] && circle.pill[3] < H, "still popping: {:?}", circle.pill);
    assert_eq!(circle.icon_alpha, 1.0, "the recording dot rides the circle from the first frame");
    assert_eq!(circle.content_alpha, 0.0, "no values yet");
    let widths: Vec<f32> = (0..=50).map(|i| island.look(now + ms(150 + i * 5), f32::INFINITY).box_w).collect();
    assert!(widths.windows(2).all(|p| p[1] >= p[0]) && widths[0] == H && widths[50] == W, "unravels left to full width");
    assert_eq!(island.content_alpha(now + OPEN), 0.0, "values wait for the open pill");
    assert!(island.content_alpha(now + OPEN + ms(100)) > 0.5);
    assert_eq!(island.look(now + OPEN + DURATION, f32::INFINITY), Look::open(Frame { w: W, h: H, alpha: 1.0 }));
    assert!(!island.animating(now + OPEN + DURATION));
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
fn hiding_fades_the_values_rolls_into_the_circle_shrinks_and_reports_hidden_once() {
    let now = Instant::now();
    let mut island = Island::new(now);
    island.update(Some((Content::Stats, W, H)), now);
    let later = now + Duration::from_secs(1);
    island.update(None, later);
    assert!(island.shown(), "still on screen while leaving");
    assert!(island.content_alpha(later + ms(75)) < 0.6 && island.content_alpha(later + ms(150)) == 0.0, "values fade first");
    assert_eq!(island.look(later + ms(150), f32::INFINITY).box_w, W, "the pill waits for them");
    let rolled = island.look(later + ms(150) + ms(250), f32::INFINITY);
    assert!((rolled.box_w - H).abs() < 0.5 && rolled.unroll < 1e-3, "rolled into the circle: {}", rolled.box_w);
    assert!(!island.tick(later + HIDE - ms(1)));
    assert!(island.tick(later + HIDE), "the circle shrank away");
    assert!(!island.shown() && island.content().is_none());
    assert!(!island.tick(later + HIDE * 2), "reported once");
}

#[test]
fn reshowing_mid_hide_unravels_again_from_where_it_is() {
    let now = Instant::now();
    let mut island = Island::new(now);
    island.update(Some((Content::Stats, W, H)), now);
    let hide = now + Duration::from_secs(1);
    island.update(None, hide);
    let mid = hide + ms(150) + ms(120);
    let here = island.look(mid, f32::INFINITY);
    assert!(here.box_w > H && here.box_w < W, "half rolled: {}", here.box_w);
    island.update(Some((Content::Stats, W, H)), mid);
    assert_eq!(island.look(mid, f32::INFINITY).box_w, here.box_w, "no jump");
    assert!(island.look(mid + ms(40), f32::INFINITY).box_w > here.box_w, "unrolling again");
    assert_eq!(island.look(mid + ms(400), f32::INFINITY).box_w, W);
}

#[test]
fn a_notice_from_nothing_carries_its_mark_in_the_circle_and_sweeps_once_open() {
    let now = Instant::now();
    let mut island = Island::new(now);
    let saved = NoticeView::new(Tone::Success, "Clip saved", "VALORANT · 0:30", true).emphasized();
    island.update(Some((Content::Notice(saved), 340.0, 58.0)), now);
    let popping = island.look(now + ms(100), f32::INFINITY);
    assert!(popping.unroll == 0.0 && popping.icon_alpha == 1.0 && popping.content_alpha == 0.0);
    assert!(island.sheen(now + ms(300)).is_none(), "no sweep across a half-open pill");
    let swept: Vec<bool> = (0..60).map(|i| island.sheen(now + ms(i * 20)).is_some()).collect();
    let first = swept.iter().position(|s| *s).expect("a sweep");
    assert!(first * 20 >= 400 + 60, "it starts once the pill is open: {} ms", first * 20);
}

#[test]
fn the_leader_pill_shows_at_once_and_the_card_unfolds_in_120_ms() {
    let now = Instant::now();
    let mut island = Island::new(now);
    island.update(Some((Content::Leader("Alt `".into()), 88.0, H)), now);
    island.update_hint(true, now);
    let first = island.look(now, f32::INFINITY);
    assert_eq!((first.unroll, first.box_w, first.content_alpha), (1.0, 88.0, 1.0), "no circle: readable at once");
    assert_eq!(island.card(now), Some(0.0));
    assert!(island.card(now + ms(40)).unwrap() > 0.5);
    assert_eq!(island.card(now + ms(120)), Some(1.0));
    assert!(!island.animating(now + ms(200)), "still while the window is open");
    let close = now + ms(1_500);
    island.update(None, close);
    island.update_hint(false, close);
    assert!(island.card(close + ms(60)).unwrap() < 1.0, "folds");
    assert!(!island.tick(close + ms(100)));
    assert!(island.tick(close + ms(150)), "gone once the pill faded and the card folded");
}

#[test]
fn over_the_stats_the_card_unfolds_without_touching_the_pill() {
    let now = Instant::now();
    let mut island = Island::new(now - Duration::from_secs(1));
    island.update(Some((Content::Stats, W, H)), now - Duration::from_secs(1));
    let before = island.look(now, f32::INFINITY);
    island.update(Some((Content::Stats, W, H)), now);
    island.update_hint(true, now);
    let with_card = island.look(now + ms(60), f32::INFINITY);
    assert_eq!((with_card.box_w, with_card.box_h, with_card.content_alpha), (before.box_w, before.box_h, 1.0));
    assert!(island.card(now + ms(60)).is_some() && island.animating(now + ms(60)));
    island.update_hint(false, now + ms(500));
    assert_eq!(island.card(now + ms(620)), None);
    assert!(island.shown(), "the stats stay");
}
