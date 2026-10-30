//! A clip save end to end through the reducer and the island: `saving`
//! (controller) → the spinning shutter ring → `saved` / `save_failed`
//! (engine) → close, fill, tick, one sweep → hold → back to the stats.

use super::*;
use crate::badge::{SETTLED, SWEEP_AFTER};
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
    json!({"mod":"engine","event":{"type":"saved","path":"V:\\c\\VALORANT__2026-10-30__21-14-03.mp4","seconds":30.0}})
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
        let status = json!({"mod":"session","event":{"type":"status","state":"recording","overlay_stats":true,"overlay":{"enabled":true}}});
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
        let next = content(self.reducer.view(), self.reducer.session(), true).map(|c| match c {
            Content::Stats => (c, STATS.0, STATS.1),
            notice => (notice, GROWN.0, GROWN.1),
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
    assert_eq!((notice.title.as_str(), notice.detail.as_str(), notice.tone), ("Saving", "0:30…", Tone::Recording));
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
    assert_eq!((done.ring, done.dot, done.tick), (ACTIVE, 2.0, 1.0));
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
fn saving_then_failed_closes_the_ring_in_the_failure_colour_and_holds_four_seconds() {
    let now = Instant::now();
    let mut rig = Rig::new(now);
    rig.send(&saving(30), now);
    rig.send(&failed(), now + ms(900));
    let notice = rig.notice().unwrap();
    assert_eq!((notice.title.as_str(), notice.tone, notice.save), ("Save failed", Tone::Failure, Some(SavePhase::Failed)));
    let done = rig.island.badge(now + ms(900) + SETTLED).unwrap();
    assert_eq!((done.ring, done.dot_color, done.dot, done.tick), (ERROR, ERROR, 1.0, 0.0));
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
    assert_eq!(rig.island.badge(now + SETTLED).unwrap().tick, 1.0, "ticked within half a second");
}

#[test]
fn a_second_save_while_saving_keeps_the_ring_and_extends_the_wait() {
    let now = Instant::now();
    let mut rig = Rig::new(now);
    rig.send(&saving(30), now);
    let alone = rig.island.badge(now + ms(800)).unwrap();
    rig.send(&saving(60), now + ms(700));
    assert_eq!(rig.notice().unwrap().detail, "1:00…", "the newer length shows");
    assert_eq!(rig.island.badge(now + ms(800)), Some(alone), "the ring is not restarted");
    rig.refresh(now + SAVING_FOR + ms(500));
    assert!(rig.notice().is_some(), "30 s from the second save");
    rig.send(&saved(), now + ms(2_000));
    assert_eq!(rig.island.badge(now + ms(2_000) + SETTLED).unwrap().tick, 1.0);
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
