//! Editor playback: play/pause/seek on the native preview, following the clock through the
//! kept ranges (the rules live in `transport_rules.rs`), and the stall watchdog.
use std::time::{Duration, Instant};

use super::EditorState;
use crate::transport_rules::{self, Follow};

impl EditorState {
    /// A seek still waiting for the preview counts as where the playhead is.
    pub fn position(&self) -> f64 {
        if let Some(at) = self.pending_seek {
            return at;
        }
        self.preview.as_ref().and_then(|p| p.position().ok()).unwrap_or(0.0)
    }

    /// The latest time the player's clock can reach: the clip's end, or the picture's last
    /// frame for a clip without sound.
    fn clock_limit(&self) -> f64 {
        self.preview.as_ref().and_then(|p| p.clock_end()).unwrap_or_else(|| self.duration())
    }

    pub fn toggle_play(&mut self) {
        if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Plays from the playhead. Inside a kept part it follows the kept parts (from the next one
    /// at a part's end, from the first one when the kept part has finished); outside them
    /// (before In, in a cut, after Out) it plays the cut footage right where it is. Pressed
    /// while the preview is still loading, playback starts as soon as it is ready.
    pub fn play(&mut self) {
        if self.info.is_none() && self.loading_path.is_none() {
            return; // no clip
        }
        let limit = self.clock_limit();
        let loading = self.loading_path.is_some() || self.pending_seek.is_some();
        let Some(preview) = self.preview.as_mut() else { return };
        if (loading || !preview.is_ready()) && preview.error().is_none() {
            self.play_when_ready = true;
            self.playing = true;
            return;
        }
        self.play_when_ready = false;
        let pos = preview.position().unwrap_or(0.0);
        let free = transport_rules::plays_free(&self.plan.ranges, pos, limit);
        let start = if free { pos } else { transport_rules::play_from(&self.plan.ranges, pos, limit) };
        let result = if (start - pos).abs() > 1e-6 { preview.seek(start) } else { Ok(()) };
        match result.and_then(|()| preview.play()) {
            Ok(()) => {
                if let Some(info) = &self.info {
                    note_played(self.generation, &info.path);
                }
                self.playing = true;
                self.free_play = free;
                self.play_started = Some(Instant::now());
                self.saw_native_playing = false;
                self.watch.reset();
            }
            Err(error) => {
                self.playing = false;
                self.status = error.clone();
                self.last_error = Some(error);
            }
        }
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.play_when_ready = false;
        self.free_play = false;
        self.loading.buffering = false;
        self.play_started = None;
        self.watch.reset();
        if let Some(preview) = self.preview.as_mut() {
            let _ = preview.pause();
        }
    }

    pub fn seek(&mut self, seconds: f64) {
        self.watch.reset();
        let target = seconds.clamp(0.0, self.duration());
        if self.pending_seek.is_some() {
            // The preview is not ready yet: this seek replaces the waiting one.
            self.pending_seek = Some(target);
            return;
        }
        if self.playing && !self.play_when_ready {
            // A seek while playing goes free, or back under the kept-range rules, by where it lands.
            self.free_play = transport_rules::plays_free(&self.plan.ranges, target, self.clock_limit());
        }
        if let Some(preview) = self.preview.as_mut() {
            if let Err(error) = preview.seek(target) {
                self.status = error;
            }
        }
    }

    pub fn nudge(&mut self, delta: f64) {
        let pos = self.position();
        self.seek(pos + delta);
    }

    /// Per-frame while playing: continue, skip a cut, or stop at the end of the kept part
    /// (playing free: continue until a kept part takes over, or stop at the clip's end).
    pub(super) fn follow_playback(&mut self) -> bool {
        let limit = self.clock_limit();
        let Some(preview) = self.preview.as_mut() else { return false };
        let Ok(pos) = preview.position() else { return false };
        let native_playing = preview.is_playing();
        crate::trace::note_underruns(self.info.as_ref().map(|i| i.path.as_str()), preview.audio_underruns());
        let audio_stalls = preview.audio_stalls.get();
        if audio_stalls != self.audio_stalls_seen {
            self.audio_stalls_seen = audio_stalls;
            let clip = self.info.as_ref().map(|i| i.path.clone());
            crate::trace::player_event("audio clock stalled: sound restarted at the video time", serde_json::json!({ "clip": clip, "position": pos }));
        }
        self.saw_native_playing |= native_playing;
        let settled = self.play_started.is_some_and(|t| t.elapsed() >= Duration::from_millis(250));
        let free = self.free_play;
        let next = if free { transport_rules::follow_free(&self.plan.ranges, pos, limit) } else { transport_rules::follow(&self.plan.ranges, pos, limit) };
        // The end a stop rests on: the kept part's, or the clip's when playing free.
        let rest = if free { Some(limit) } else { self.plan.ranges.last().map(|r| r.end.min(limit)) };
        match next {
            Follow::Enter => self.free_play = false,
            Follow::Stop => {
                self.pause();
                // The clock overshoots by up to a poll: rest the playhead on the end.
                if let Some(end) = rest.filter(|end| pos > *end) {
                    self.seek(end);
                }
            }
            Follow::Jump(next) => self.seek(next),
            // The clock stopped by itself: the sound ran out, or a clip without sound reached its
            // last frame (the video player then stops and reads 0: rest the playhead on the end).
            Follow::Keep if settled && self.saw_native_playing && !native_playing => {
                let silent = !preview.has_sound();
                self.pause();
                if let Some(end) = rest.filter(|_| silent) {
                    self.seek(end);
                }
            }
            Follow::Keep if settled => self.watch_stalls(pos),
            Follow::Keep => {}
        }
        true
    }

    /// "Playing" but the clock froze: restart at the same spot; give up after repeated stalls.
    fn watch_stalls(&mut self, pos: f64) {
        use crate::playback_watch::WatchAction;
        let action = self.watch.observe(pos, Instant::now());
        if action == WatchAction::Nothing {
            return;
        }
        let clip = self.info.as_ref().map(|i| i.path.clone());
        let what = if action == WatchAction::Restart { "stall: restarting playback" } else { "stall: gave up" };
        crate::trace::player_event(what, serde_json::json!({ "clip": clip, "position": pos }));
        if action == WatchAction::GiveUp {
            self.pause();
            self.status = "Playback stopped responding. Press play again, or reopen the clip.".into();
            return;
        }
        if let Some(preview) = self.preview.as_mut() {
            let _ = preview.pause();
            let _ = preview.seek(pos);
            let _ = preview.play();
        }
        self.play_started = Some(Instant::now());
        self.saw_native_playing = false;
    }
}

/// The smart cache hears of the first play of each open (`Interaction::Played`).
fn note_played(generation: u64, path: &str) {
    static REPORTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);
    if REPORTED.swap(generation, std::sync::atomic::Ordering::Relaxed) != generation {
        crate::services::storage::record_interaction(crate::services::storage::Interaction::Played(path.to_owned()));
    }
}
