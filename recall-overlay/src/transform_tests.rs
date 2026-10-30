//! The action notice's transformation: grow (both dimensions, one spring
//! overshoot), one sweep, then back to the stats or collapse away.

use super::*;
use crate::anim::SPRING_FOR;
use crate::cue::{CUE_DELAY, CUE_SWEEP};
use crate::layout::{ACTIVE, ERROR};
use crate::notice::Tone;
use std::time::Duration;

/// The stats pill and the larger notice pill it transforms into.
const STATS: (f32, f32) = (300.0, 44.0);
const GROWN: (f32, f32) = (340.0, 58.0);

fn saved() -> Content {
    Content::Notice(NoticeView::new(Tone::Success, "Clip saved", "VALORANT · 0:30", true).emphasized())
}

fn failed() -> Content {
    Content::Notice(NoticeView::new(Tone::Failure, "Save failed", "disk full", false).emphasized())
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// An island showing the settled stats pill at `now`.
fn stats_island(now: Instant) -> Island {
    let mut island = Island::new(now - Duration::from_secs(1));
    island.update(Some((Content::Stats, STATS.0, STATS.1)), now - Duration::from_secs(1));
    assert_eq!(island.frame(now), Frame { w: STATS.0, h: STATS.1, alpha: 1.0 });
    island
}

/// Frames every 5 ms over `[from, from + span]`.
fn path(island: &Island, from: Instant, span: Duration) -> Vec<Frame> {
    (0..=span.as_millis() as u32 / 5).map(|i| island.frame(from + ms(i as u64 * 5))).collect()
}

#[test]
fn an_action_notice_grows_the_stats_pill_in_both_dimensions_with_one_overshoot() {
    let now = Instant::now();
    let mut island = stats_island(now);
    island.update(Some((saved(), GROWN.0, GROWN.1)), now);
    assert_eq!(island.frame(now), Frame { w: STATS.0, h: STATS.1, alpha: 1.0 }, "starts from the stats pill");
    let frames = path(&island, now, SPRING_FOR);
    let peak_w = frames.iter().map(|f| f.w).fold(0.0, f32::max);
    let peak_h = frames.iter().map(|f| f.h).fold(0.0, f32::max);
    assert!(peak_w > GROWN.0 + 3.0 && peak_w < GROWN.0 + 8.0, "springs past the new width once: {peak_w}");
    assert!(peak_h > GROWN.1 + 1.0 && peak_h < GROWN.1 + 3.0, "and past the new height: {peak_h}");
    assert!(frames.iter().all(|f| f.w >= STATS.0 && f.h >= STATS.1), "never dips under the stats pill");
    assert!(frames.iter().all(|f| f.alpha == 1.0), "the window stays opaque: a shape change, not a fade");
    assert_eq!(island.frame(now + SPRING_FOR), Frame { w: GROWN.0, h: GROWN.1, alpha: 1.0 }, "settles exactly");
    assert!(island.animating(now + ms(300)));
}

#[test]
fn after_the_hold_the_notice_eases_back_to_the_stats_pill_without_bouncing() {
    let now = Instant::now();
    let mut island = stats_island(now);
    island.update(Some((saved(), GROWN.0, GROWN.1)), now);
    let back = now + Duration::from_secs(3);
    assert!(!island.animating(back), "idle during the hold");
    island.update(Some((Content::Stats, STATS.0, STATS.1)), back);
    let frames = path(&island, back, DURATION);
    assert!(frames.windows(2).all(|p| p[1].w <= p[0].w && p[1].h <= p[0].h), "shrinks straight back, no overshoot");
    assert_eq!(island.frame(back + DURATION), Frame { w: STATS.0, h: STATS.1, alpha: 1.0 });
    assert_eq!(island.content(), Some(&Content::Stats));
    assert!(island.sheen(back + ms(100)).is_none(), "the hand-back has no sweep");
    assert!(!island.animating(back + DURATION));
}

#[test]
fn with_no_stats_the_notice_unravels_from_a_circle_without_a_spring_and_rolls_away() {
    let now = Instant::now();
    let mut island = Island::new(now);
    island.update(Some((saved(), GROWN.0, GROWN.1)), now);
    assert_eq!(island.frame(now), Frame { w: GROWN.0, h: GROWN.1, alpha: 1.0 }, "no size spring from nothing");
    let looks: Vec<_> = (0..=80).map(|i| island.look(now + ms(i * 5), f32::INFINITY)).collect();
    assert!(looks[..30].iter().all(|l| l.unroll == 0.0 && l.box_w == GROWN.1), "a circle first, 58 px");
    assert!(looks.iter().all(|l| l.box_w <= GROWN.0), "the pill never overshoots its width");
    assert_eq!(looks[80].box_w, GROWN.0, "open at 400 ms");
    let hide = now + Duration::from_secs(3);
    island.update(None, hide);
    assert!(!island.tick(hide + ms(500)));
    assert!(island.tick(hide + ms(550)), "faded, rolled up and shrunk away by 550 ms");
    assert!(!island.shown() && island.sheen(hide + ms(550)).is_none());
}

#[test]
fn a_second_notice_mid_transformation_carries_on_without_a_jump_or_a_second_sweep() {
    let now = Instant::now();
    let mut island = stats_island(now);
    island.update(Some((saved(), GROWN.0, GROWN.1)), now);
    let mid = now + ms(100);
    let (here, moving) = (island.frame(mid), island.frame(mid + ms(5)).w - island.frame(mid).w);
    let sweep = island.sheen(mid).expect("sweeping");
    island.update(Some((failed(), 320.0, GROWN.1)), mid);
    assert_eq!(island.frame(mid), here, "no jump");
    let still = island.frame(mid + ms(5)).w - here.w;
    assert!(still * moving > 0.0 && (still - moving).abs() < moving.abs() * 0.5, "keeps moving: {still} vs {moving}");
    assert_eq!(island.sheen(mid), Some(sweep), "the running sweep is not restarted");
    assert_eq!(island.content_alpha(mid), 0.0, "the new content fades in");
    assert_eq!(island.frame(mid + SPRING_FOR), Frame { w: 320.0, h: GROWN.1, alpha: 1.0 });
    // A notice after the sweep ended gets its own, in its own colour.
    let later = now + Duration::from_secs(1);
    island.update(Some((saved(), GROWN.0, GROWN.1)), later);
    let next = island.sheen(later + CUE_DELAY + CUE_SWEEP / 2).expect("a new sweep");
    assert_eq!(next.color, ACTIVE);
}

#[test]
fn the_sweep_runs_once_in_the_tone_colour_and_the_hold_is_idle() {
    let now = Instant::now();
    let mut island = stats_island(now);
    island.update(Some((failed(), GROWN.0, GROWN.1)), now);
    assert_eq!(island.sheen(now), None, "waits for the pill to start growing");
    let sweep = island.sheen(now + CUE_DELAY + ms(10)).expect("sweeping");
    assert_eq!(sweep.color, ERROR, "a failure sweeps in the error colour");
    let done = now + CUE_DELAY + CUE_SWEEP;
    let seen: Vec<bool> = (0..=150).map(|i| island.sheen(now + ms(i * 20)).is_some()).collect();
    let passes = seen.windows(2).filter(|p| !p[0] && p[1]).count();
    assert_eq!(passes, 1, "exactly one pass in 3 s");
    // The 4 Hz ticks re-send the same notice: no restart, nothing to draw.
    for tick in 1..=8u64 {
        island.update(Some((failed(), GROWN.0, GROWN.1)), now + ms(250 * tick));
    }
    assert_eq!(island.sheen(done + ms(500)), None);
    assert!(!island.animating(done), "nothing animates after the sweep and the spring");
}

#[test]
fn session_notices_and_the_chord_hint_keep_the_plain_swap() {
    let now = Instant::now();
    let game = Content::Notice(NoticeView::new(Tone::Recording, "Recording", "VALORANT", true));
    let chord = NoticeView { chord: true, ..NoticeView::new(Tone::Neutral, "1 Bookmark", "4 Record", false) };
    for plain in [game, Content::Notice(chord)] {
        let mut island = stats_island(now);
        island.update(Some((plain, 200.0, STATS.1)), now);
        let frames = path(&island, now, DURATION);
        assert!(frames.windows(2).all(|p| p[1].w <= p[0].w), "eases straight to size: no spring");
        assert_eq!(island.frame(now + DURATION).w, 200.0);
        assert!((0..40).all(|i| island.sheen(now + ms(i * 20)).is_none()), "no sweep");
        assert!(!island.animating(now + DURATION));
    }
}

/// Every char 0.6 em wide (as `layout_tests`' fake).
struct Fake;

impl Measure for Fake {
    fn measure(&self, role: crate::layout::Role, scale: f32, text: &str) -> crate::layout::TextMetrics {
        let em = role.size() * scale;
        crate::layout::TextMetrics { width: text.chars().count() as f32 * em * 0.6, height: em * 1.25, baseline: em * 0.95, cap: em * 0.7 }
    }
}

#[test]
fn an_action_notice_lays_out_grown_and_over_the_stats_but_others_at_their_own_size() {
    use crate::placement::{grow, DEFAULT_SPOT, NOTICE_H};
    let Content::Notice(saved) = saved() else { unreachable!() };
    let g = grow(&DEFAULT_SPOT);
    let (scene, scale, size) = notice_layout(&saved, 1.5, g, || Some(450.0), &Fake);
    assert_eq!(scale, 1.5 * g, "text and marks grow with the pill");
    assert!((scene.h - NOTICE_H * 1.5).abs() < 1e-3, "58 px at 1440p, scaled: {}", scene.h);
    assert_eq!(size, (450.0 + GROW_W * 1.5, NOTICE_H * 1.5), "wider than the stats under it");
    let (_, _, alone) = notice_layout(&saved, 1.0, g, || None, &Fake);
    assert_eq!(alone, (notice_scene(&saved, g, &Fake).w, NOTICE_H), "no stats under it: its own grown size, whole px");
    let game = NoticeView::new(Tone::Recording, "Recording", "VALORANT", true);
    let (scene, scale, size) = notice_layout(&game, 1.0, g, || panic!("a plain notice never sizes over the stats"), &Fake);
    assert_eq!((scale, size), (1.0, (scene.w, 44.0)));
}

#[test]
fn action_results_and_failures_transform_the_island_and_session_changes_do_not() {
    use crate::reducer::Reducer;
    use serde_json::json;
    let now = Instant::now();
    let shows = |envelope: serde_json::Value| {
        let mut reducer = Reducer::new();
        let status = json!({"mod":"session","event":{"type":"status","state":"recording","overlay":{"enabled":true}}});
        reducer.apply_envelope(&status, now);
        reducer.apply_envelope(&envelope, now);
        match content(reducer.view(), reducer.session(), true) {
            Some(Content::Notice(notice)) => (notice.title.clone(), notice.emphasis),
            other => panic!("no notice for {envelope}: {other:?}"),
        }
    };
    let engine = |event: serde_json::Value| json!({"mod":"engine","event":event});
    let session = |kind: &str| json!({"mod":"session","event":{"type":"notice","kind":kind,"title":"","detail":"VALORANT"}});
    for event in [
        json!({"type":"saved","path":"V:\\c\\VALORANT__x.mp4","seconds":30.0}),
        json!({"type":"save_failed","reason":"disk full"}),
        json!({"type":"bookmarked"}),
        json!({"type":"screenshot","path":"V:\\c\\shot.png"}),
        json!({"type":"screenshot_failed","reason":"x"}),
        json!({"type":"recording_started","path":"V:\\c\\r.mp4"}),
        json!({"type":"recording_stopped","path":"V:\\c\\r.mp4","seconds":61.0}),
        json!({"type":"recording_failed","reason":"x"}),
        json!({"type":"status","encoder":"failed","encoder_error":"NVENC init failed"}),
    ] {
        let (title, emphasis) = shows(engine(event));
        assert!(emphasis, "{title} transforms the island");
    }
    assert!(shows(session("encoder")).1, "the controller's encoder failure too");
    for kind in ["game", "desktop", "paused", "waiting"] {
        assert!(!shows(session(kind)).1, "{kind}: plain swap");
    }
    assert!(!shows(engine(json!({"type":"mic_muted","muted":true}))).1, "mic toggle: plain swap");
    let chord = json!({"mod":"session","event":{"type":"chord","active":true,"options":[{"key":"1","label":"Bookmark"}]}});
    assert!(!shows(chord).1, "chord hint: plain swap");
}

#[test]
fn the_grown_pill_is_bigger_than_the_stats_pill_in_both_dimensions() {
    let narrow = Scene { w: 264.0, h: 58.0, ..Scene::default() };
    assert_eq!(expanded(&narrow, Some(300.0), 1.0), (340.0, 58.0), "at least GROW_W wider than the stats");
    assert_eq!(expanded(&narrow, Some(300.0), 1.5), (360.0, 58.0), "GROW_W scales with the island");
    let wide = Scene { w: 441.0, h: 58.0, ..Scene::default() };
    assert_eq!(expanded(&wide, Some(300.0), 1.0), (441.0, 58.0), "long content keeps its own width");
    assert_eq!(expanded(&narrow, None, 1.0), (264.0, 58.0), "no stats under it: its own size");
}
