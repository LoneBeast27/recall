//! A clip save end to end through the reducer and the island: `saving`
//! (controller) → the spinning shutter ring → `saved` / `save_failed`
//! (engine) → close, fill, tick, one sweep → hold → back to the stats.

use super::*;
use crate::badge::{Glyph, SETTLED, SWEEP_AFTER};
use crate::layout::{ACTIVE, ERROR, SIGNAL};
use crate::notice::{SavePhase, Tone};
use crate::reducer::{Reducer, SAVING_FOR};
use serde_json::{json, Value};
use std::time::Duration;

const STATS: (f32, f32) = (300.0, 44.0);
const GROWN: (f32, f32) = (340.0, 58.0);

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn saving(seconds: u64) -> Value {
    json!({"mod":"session","event":{"type":"saving","seconds":seconds}})
}

fn saved() -> Value {
    json!({"mod":"engine","event":{"type":"saved","path":"E:\\c\\VALORANT__2026-10-30__21-14-03.mp4","seconds":30.0}})
}

fn failed() -> Value {
    json!({"mod":"engine","event":{"type":"save_failed","reason":"disk full"}})
}

/// The reducer and the island as the app drives them (one refresh per call).
struct Rig {
    reducer: Reducer,
    island: Island,
}

impl Rig {
    /// Stats on and showing, settled, at `now`.
    fn new(now: Instant) -> Self {
        let mut rig = Self { reducer: Reducer::new(), island: Island::new(now - Duration::from_secs(1)) };
        let status = json!({"mod":"session","event":{"type":"status","state":"recording","overlay_stats":true,"overlay":{"enabled":true},
            "target":{"name":"VALORANT","exe":"VALORANT-Win64-Shipping.exe","pid":42}}});
        rig.reducer.apply_envelope(&status, now - Duration::from_secs(1));
        rig.refresh(now - Duration::from_secs(1));
        rig
    }

    fn send(&mut self, envelope: &Value, now: Instant) {
        self.reducer.apply_envelope(envelope, now);
        self.refresh(now);
    }

    fn refresh(&mut self, now: Instant) {
        self.reducer.tick(now);
        // Notices size like the layout: at least the grown floor, wider for long text.
        let next = content(self.reducer.view(), self.reducer.session(), true).map(|c| match c {
            Content::Stats => (c, STATS.0, STATS.1),
            Content::Notice(n) => {
                let chars = n.title.chars().count().max(n.detail.chars().count()) as f32;
                let w = GROWN.0.max(120.0 + 9.0 * chars);
                (Content::Notice(n), w, GROWN.1)
            }
            leader => (leader, 88.0, STATS.1),
        });
        self.island.update(next, now);
        self.island.tick(now);
    }

    fn notice(&self) -> Option<NoticeView> {
        match self.island.content() {
            Some(Content::Notice(n)) => Some(n.clone()),
            _ => None,
        }
    }
}

#[test]
fn saving_spins_the_ring_then_saved_closes_fills_ticks_and_sweeps_once() {
    let now = Instant::now();
    let mut rig = Rig::new(now);
    rig.send(&saving(30), now);
    let notice = rig.notice().expect("the saving pill");
    assert_eq!((notice.title.as_str(), notice.detail.as_str(), notice.tone), ("Saving", "VALORANT · 0:30", Tone::Recording));
    assert!(notice.game_font && notice.emphasis && notice.save == Some(SavePhase::Saving));
    assert_eq!(rig.island.frame(now + ms(420)), Frame { w: GROWN.0, h: GROWN.1, alpha: 1.0 }, "transformed at once");
    let ring = rig.island.badge(now + ms(500)).expect("the ring");
    assert_eq!((ring.ring, ring.dot), (SIGNAL, 1.0));
    assert!((0..90).all(|i| rig.island.sheen(now + ms(i * 20)).is_none()), "no sweep while saving");
    assert!(rig.island.animating(now + ms(1_700)), "the ring keeps spinning");
    let landed = now + ms(1_800);
    let spinning = rig.island.badge(landed).unwrap();
    rig.send(&saved(), landed);
    let notice = rig.notice().unwrap();
    assert_eq!((notice.title.as_str(), notice.save), ("Clip saved", Some(SavePhase::Saved)));
    assert_eq!(rig.island.badge(landed), Some(spinning), "the ring carries on: no jump");
    assert_eq!(rig.island.look(landed + ms(10), f32::INFINITY).icon_alpha, 1.0, "the mark never blinks");
    let done = rig.island.badge(landed + SETTLED).unwrap();
    assert_eq!((done.ring, done.dot, done.drawn), (ACTIVE, 2.0, 1.0));
    let first = (0..100).position(|i| rig.island.sheen(landed + ms(i * 10)).is_some()).expect("one sweep") as u64 * 10;
    let after = SWEEP_AFTER.as_millis() as u64;
    assert!((after..after + 20).contains(&first), "right after the fill (340 ms): {first} ms");
    rig.refresh(landed + ms(2_999));
    assert!(rig.notice().is_some(), "held SAVED_FOR from the saved event");
    rig.refresh(landed + ms(3_000));
    assert_eq!(rig.island.content(), Some(&Content::Stats));
    assert!(rig.island.badge(landed + ms(3_000)).is_none());
}

#[test]
fn saving_then_failed_settles_with_a_bang_in_the_failure_colour_and_holds_four_seconds() {
    let now = Instant::now();
    let mut rig = Rig::new(now);
    rig.send(&saving(30), now);
    rig.send(&failed(), now + ms(900));
    let notice = rig.notice().unwrap();
    assert_eq!((notice.title.as_str(), notice.tone, notice.save), ("Save failed", Tone::Failure, Some(SavePhase::Failed)));
    let done = rig.island.badge(now + ms(900) + SETTLED).unwrap();
    assert_eq!((done.ring, done.dot_color, done.dot, done.glyph, done.drawn), (ERROR, ERROR, 2.0, Glyph::Bang, 1.0));
    rig.refresh(now + ms(900 + 3_999));
    assert!(rig.notice().is_some());
    rig.refresh(now + ms(900 + 4_000));
    assert_eq!(rig.island.content(), Some(&Content::Stats));
}

#[test]
fn a_saving_with_no_outcome_ends_quietly_after_thirty_seconds() {
    let now = Instant::now();
    let mut rig = Rig::new(now);
    let thirty = Duration::from_secs(30);
    assert_eq!(SAVING_FOR, thirty);
    rig.send(&saving(30), now);
    rig.refresh(now + thirty - ms(1));
    assert!(rig.notice().is_some(), "still saving at 29.999 s");
    rig.refresh(now + thirty);
    assert_eq!(rig.island.content(), Some(&Content::Stats), "back to the stats, no failure shown");
    assert!(rig.island.badge(now + thirty).is_none() && rig.island.sheen(now + thirty + ms(500)).is_none());
}

#[test]
fn a_saved_with_no_saving_first_plays_the_settle_from_the_dot() {
    let now = Instant::now();
    let mut rig = Rig::new(now);
    rig.send(&saved(), now);
    assert_eq!(rig.notice().unwrap().save, Some(SavePhase::Saved));
    assert_eq!(rig.island.badge(now).unwrap().sweep, 0.0, "from the plain dot");
    assert_eq!(rig.island.badge(now + SETTLED).unwrap().drawn, 1.0, "ticked within half a second");
}

#[test]
fn a_second_save_while_saving_keeps_the_ring_and_extends_the_wait() {
    let now = Instant::now();
    let mut rig = Rig::new(now);
    rig.send(&saving(30), now);
    let alone = rig.island.badge(now + ms(800)).unwrap();
    rig.send(&saving(60), now + ms(700));
    assert_eq!(rig.notice().unwrap().detail, "VALORANT · 1:00", "the newer length shows");
    assert_eq!(rig.island.badge(now + ms(800)), Some(alone), "the ring is not restarted");
    rig.refresh(now + SAVING_FOR + ms(500));
    assert!(rig.notice().is_some(), "30 s from the second save");
    rig.send(&saved(), now + ms(2_000));
    assert_eq!(rig.island.badge(now + ms(2_000) + SETTLED).unwrap().drawn, 1.0);
}

#[test]
fn from_nothing_the_saving_ring_shows_in_the_circle_before_the_pill_opens() {
    let now = Instant::now();
    let (mut reducer, mut island) = (Reducer::new(), Island::new(now));
    let status = json!({"mod":"session","event":{"type":"status","state":"recording","overlay_stats":false,"overlay":{"enabled":true}}});
    reducer.apply_envelope(&status, now);
    reducer.apply_envelope(&saving(30), now);
    let Some(Content::Notice(n)) = content(reducer.view(), reducer.session(), true) else { panic!("saving pill") };
    island.update(Some((Content::Notice(n), 200.0, GROWN.1)), now);
    let circle = island.look(now + ms(120), f32::INFINITY);
    assert!(circle.unroll == 0.0 && circle.icon_alpha == 1.0, "still the circle, mark showing");
    assert!(island.badge(now + ms(120)).unwrap().sweep > 0.0, "the ring is already drawing in");
}

#[test]
fn when_the_clip_lands_only_the_title_changes_and_nothing_moves() {
    let now = Instant::now();
    let mut rig = Rig::new(now);
    rig.send(&saving(30), now);
    let landed = now + ms(1_800);
    let before = rig.island.look(landed - ms(1), f32::INFINITY);
    rig.send(&saved(), landed);
    assert_eq!(rig.notice().unwrap().detail, "VALORANT · 0:30", "the same detail line");
    for t in [0, 50, 100, 200, 420] {
        let look = rig.island.look(landed + ms(t), f32::INFINITY);
        assert_eq!((look.box_w, look.box_h, look.full_w), (before.box_w, before.box_h, before.full_w), "no resize at {t} ms");
        assert_eq!((look.content_alpha, look.icon_alpha), (1.0, 1.0), "detail and mark stay lit at {t} ms");
    }
    assert_eq!(rig.island.look(landed, f32::INFINITY).title_alpha, 0.0, "the new title word fades in");
    assert!(rig.island.look(landed + ms(100), f32::INFINITY).title_alpha > 0.5);
    assert!(rig.island.sheen(landed + ms(100)).is_none() && rig.island.badge(landed).is_some());
}

#[test]
fn a_saved_clip_for_another_game_springs_to_its_width_without_a_jump() {
    let now = Instant::now();
    let mut island = Island::new(now - Duration::from_secs(1));
    let stage = |phase, title: &str, detail: &str| {
        let tone = if phase == SavePhase::Saving { Tone::Recording } else { Tone::Success };
        Content::Notice(NoticeView::new(tone, title, detail, true).emphasized().saving(phase))
    };
    island.update(Some((stage(SavePhase::Saving, "Saving", "VALORANT · 0:30"), 300.0, 58.0)), now - Duration::from_secs(1));
    let landed = now;
    island.update(Some((stage(SavePhase::Saved, "Clip saved", "Rocket League · 0:30"), 360.0, 58.0)), landed);
    assert_eq!(island.frame(landed).w, 300.0, "starts from the saving width");
    let widths: Vec<f32> = (0..=84).map(|i| island.frame(landed + ms(i * 5)).w).collect();
    let step = widths.windows(2).map(|p| (p[1] - p[0]).abs()).fold(0.0, f32::max);
    assert!(step < 8.0, "continuous: largest 5 ms step {step}");
    assert_eq!(island.frame(landed + crate::anim::SPRING_FOR).w, 360.0);
    let look = island.look(landed, f32::INFINITY);
    assert_eq!((look.content_alpha, look.icon_alpha, look.detail_alpha, look.title_alpha), (1.0, 1.0, 0.0, 0.0), "only the new lines fade in");
}

fn failed_reason() -> Value {
    json!({"mod":"engine","event":{"type":"save_failed","reason":"the output folder is unavailable"}})
}

/// One sampled moment of a run.
#[derive(Clone, Debug, PartialEq)]
struct Snap {
    stage: &'static str,
    look: Look,
    badge: Option<crate::badge::Badge>,
    sheen: Option<f32>,
}

fn snap(rig: &Rig, now: Instant) -> Snap {
    let stage = match rig.island.content() {
        _ if !rig.island.shown() => "hidden",
        Some(Content::Stats) => "stats",
        Some(Content::Notice(n)) if n.save == Some(SavePhase::Saving) => "saving",
        Some(Content::Notice(_)) => "landed",
        Some(Content::Leader(_)) => "hint",
        None => "hidden",
    };
    let look = rig.island.look(now, f32::INFINITY);
    Snap { stage, look, badge: rig.island.badge(now), sheen: rig.island.sheen(now).map(|s| s.progress) }
}

/// saving at 0, `outcome` at 1.8 s, sampled every 10 ms to 6.5 s.
fn run(outcome: &Value, now: Instant) -> Vec<Snap> {
    let mut rig = Rig::new(now);
    rig.send(&saving(30), now);
    (0..=650u64)
        .map(|i| {
            let t = now + ms(i * 10);
            if i == 180 { rig.send(outcome, t) } else { rig.refresh(t) }
            snap(&rig, t)
        })
        .collect()
}

fn stages(run: &[Snap]) -> Vec<&'static str> {
    let mut seq: Vec<&'static str> = run.iter().map(|s| s.stage).collect();
    seq.dedup();
    seq
}

/// The look minus what may differ between outcomes (width for the reason,
/// the detail line's text fade).
fn motion(look: &Look) -> [f32; 8] {
    [look.box_h, look.margin, look.pop, look.unroll, look.alpha, look.content_alpha, look.title_alpha, look.icon_alpha]
}

#[test]
fn saved_and_failed_take_one_path_differing_only_in_colour_glyph_text_width_and_hold() {
    let now = Instant::now();
    let (ok, bad) = (run(&saved(), now), run(&failed_reason(), now));
    assert_eq!(ok[..180], bad[..180], "identical up to the outcome");
    assert_eq!(stages(&ok), stages(&bad), "the same states in the same order");
    assert_eq!(stages(&ok), vec!["saving", "landed", "stats"]);
    let back = |run: &[Snap]| run.iter().position(|s| s.stage == "stats").unwrap();
    let (ok_back, bad_back) = (back(&ok), back(&bad));
    assert_eq!((ok_back, bad_back), (480, 580), "hand-back after 3 s / 4 s from the outcome");
    for i in 180..ok_back {
        let (a, b) = (&ok[i], &bad[i]);
        assert_eq!(motion(&a.look), motion(&b.look), "same motion at {} ms", i * 10);
        let (x, y) = (a.badge.unwrap(), b.badge.unwrap());
        assert_eq!((x.start, x.sweep, x.dot, x.drawn), (y.start, y.sweep, y.dot, y.drawn), "same mark at {} ms", i * 10);
        assert_eq!(a.sheen, b.sheen, "same sweep timing at {} ms", i * 10);
    }
    assert_eq!((ok[200].badge.unwrap().glyph, bad[200].badge.unwrap().glyph), (Glyph::Tick, Glyph::Bang));
    assert!(bad[195].look.box_w > ok[195].look.box_w && bad[180].look.box_w == ok[180].look.box_w, "the reason widens it smoothly");
    // The hand-back: the same morph, step for step.
    let progress = |run: &[Snap], i: usize, from: usize| {
        let (start, end) = (run[from - 1].look.box_w, run[from + 40].look.box_w);
        (run[i].look.box_w - end) / (start - end)
    };
    for j in 0..40 {
        let (a, b) = (&ok[ok_back + j], &bad[bad_back + j]);
        assert_eq!(motion(&a.look), motion(&b.look), "same hand-back {} ms in", j * 10);
        let (pa, pb) = (progress(&ok, ok_back + j, ok_back), progress(&bad, bad_back + j, bad_back));
        assert!((pa - pb).abs() < 1e-3, "same width progress {} ms in: {pa} vs {pb}", j * 10);
    }
}
