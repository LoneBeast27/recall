//! A static game for 5 minutes while a long recording runs, through the engine's own
//! per-turn pieces composed as the engine runs them:
//! - the capture watchdog's [`tick`] over the fake slot (`App::watch_capture`);
//! - the live encoder's reopen plan, the real [`Reopen`] driven as `encoder/live.rs` does
//!   (`encoded` per frame, `status` each loop turn: only a real open or encode error can
//!   ask for a capture restart, and none comes here);
//! - the real long-recording state machine, [`Recorder`] on the fake sink, driven as the
//!   `recall-longrec` writer does (every packet, game audio, `tick` each turn).
//!
//! Frames come for 10 s, then none for 300 s (a menu, a load screen, a paused game: the
//! encoder is VFR and WGC sends no repeats), then again for 20 s.

use std::path::PathBuf;

use super::*;
use crate::encoder::reopen::Reopen;
use crate::longrec::machine::{Event, Recorder};
use crate::longrec::test_sink::{pkt, temp_dir, tracks, Call, FakeFactory, Log};

/// One encoder loop turn (its frame wait is 100 ms).
const TURN_MS: u64 = 100;
/// 60 fps: six frames a turn, a keyframe every second (the GOP).
const FRAMES_PER_TURN: u64 = 6;
const GOP: u64 = 60;

struct Engine {
    t0: Instant,
    slot: Fake,
    reopen: Reopen,
    rec: Recorder<FakeFactory>,
    log: Log,
    events: Vec<Event>,
    frames: u64,
    /// Turns the encoder reported `recovering` / `failed` (`Reopen::status`).
    recovering_turns: u32,
}

impl Engine {
    /// A running capture, an open encoder and a long recording armed at t0.
    fn start(path: PathBuf) -> Engine {
        let t0 = Instant::now();
        let mut slot = Fake { capture: true, start_ok: true, screen: SHOWN, ..Default::default() };
        slot.backoff.started(t0);
        let mut reopen = Reopen::default();
        reopen.opened(t0);
        let log = Log::default();
        let mut rec = Recorder::new(FakeFactory { log: log.clone(), no_pcm: true, ..Default::default() }, tracks());
        let events = rec.start(path, Some((0, t0)), t0);
        Engine { t0, slot, reopen, rec, log, events, frames: 0, recovering_turns: 0 }
    }

    /// Turn `n`: this turn's frames (when the game sends any), then the per-turn ticks.
    fn turn(&mut self, n: u64, frames_come: bool) {
        let now = self.t0 + Duration::from_millis(n * TURN_MS);
        if frames_come {
            for i in 0..FRAMES_PER_TURN {
                let pts_us = n * TURN_MS * 1_000 + i * 16_666;
                self.reopen.encoded(now);
                let key = self.frames.is_multiple_of(GOP);
                self.events.extend(self.rec.video(pts_us, key, pkt(self.frames as u8), false, now));
                self.frames += 1;
            }
        }
        // The game's audio plays on through a still picture (the mic track stays silent).
        self.events.extend(self.rec.audio(0, &[0.1; 9_600], now));
        if self.reopen.status().is_some() {
            self.recovering_turns += 1;
        }
        tick(&mut self.slot, now);
        self.events.extend(self.rec.tick(now));
    }

    fn assert_untouched(&self, when: &str) {
        assert_eq!((self.slot.stops, self.slot.starts), (0, 0), "{when}: capture restarts");
        assert_eq!(self.recovering_turns, 0, "{when}: the encoder left recording");
        assert!(!self.reopen.recovering() && self.reopen.reopen_failures() == 0, "{when}: encoder reopens");
        assert!(self.rec.is_active(), "{when}: the long recording runs");
        assert!(self.events.iter().all(|e| matches!(e, Event::Started { .. })), "{when}: {:?}", self.events);
        let closed = self.log.calls().into_iter().filter(|c| matches!(c, Call::Finish | Call::Abandon | Call::Open(_))).count();
        assert_eq!(closed, 1, "{when}: one part opened, none closed");
    }
}

#[test]
fn a_static_game_for_5_minutes_during_a_long_recording_restarts_nothing_and_keeps_one_file() {
    let dir = temp_dir("static-game");
    let path = dir.join("G__2026-11-07__12-00-00__rec.mp4");
    let mut e = Engine::start(path.clone());
    let still = 100; // 10 s of frames
    let back = still + 3_000; // then 300 s with none
    for n in 0..back + 200 {
        e.turn(n, !(still..back).contains(&n));
        if n == back - 1 {
            e.assert_untouched("300 s without a frame");
        }
    }
    e.assert_untouched("20 s after the frames came back");
    assert_eq!(e.events, [Event::Started { path: path.clone() }]);
    let stopped = e.rec.stop();
    assert!(
        matches!(&stopped[..], [Event::Stopped { path: p, seconds, .. }] if *p == path && (seconds - 330.0).abs() < 0.2),
        "one file of the whole 330 s: {stopped:?}"
    );
    // The last packet before the still picture lasts until the first one after it.
    let longest = e.log.videos().iter().map(|v| v.1).max().unwrap_or(0);
    assert!((longest as f64 / 1e6 - 300.0).abs() < 0.1, "the still picture is one 300 s frame: {longest} us");
    let _ = std::fs::remove_dir_all(&dir);
}
