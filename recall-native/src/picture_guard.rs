//! Picture guard: notices when the sound clock is playing but the video player sits paused, and
//! asks for the picture to be played again. Pure logic; `NativePreview::position` acts on it.
//!
//! Why: with sound, the clock is the sound's, and the editor's stall watchdog and "playing"
//! state read that clock, never the video player's. When Play is pressed the moment a clip is
//! ready, three Media Foundation requests go out in one frame: the first-frame prime's Pause,
//! the seek to the first kept part, and Play. Media Foundation finishes them asynchronously, and
//! the prime's Pause (or the paused seek's own pause) can land after Play. The video player then
//! rests paused while the sound plays on. Nothing played the picture again: the drift check only
//! re-seeked it every 500 ms, which showed one new frame per seek ("about a frame every few
//! seconds" on a 1440p clip), until a pause and play sent Play again.
use std::time::{Duration, Instant};

/// A paused picture under a playing sound clock for this long gets Play again. Longer than a
/// normal Play takes to land after a seek, short enough that a stuck start heals in under half a
/// second.
pub const PAUSED_FOR: Duration = Duration::from_millis(400);

/// What the transport sees on one clock read while the sound plays.
#[derive(Clone, Copy, Debug)]
pub struct Seen {
    /// The sound clock is running.
    pub sound_playing: bool,
    /// The video player reports Paused (not Playing, not Stopped at its end).
    pub picture_paused: bool,
    /// The sound is past the picture's last frame: the picture rests there on purpose.
    pub past_picture_end: bool,
}

#[derive(Default)]
pub struct PictureGuard {
    paused_since: std::cell::Cell<Option<Instant>>,
}

impl PictureGuard {
    /// Play, pause, seek or a new clip: start the grace time again.
    pub fn reset(&self) {
        self.paused_since.set(None);
    }

    /// True when the picture should be sent Play now.
    pub fn observe(&self, seen: Seen, now: Instant) -> bool {
        if !seen.sound_playing || !seen.picture_paused || seen.past_picture_end {
            self.paused_since.set(None);
            return false;
        }
        match self.paused_since.get() {
            Some(since) if now.duration_since(since) >= PAUSED_FOR => {
                // Measured again from this Play: at most one Play per grace time.
                self.paused_since.set(Some(now));
                true
            }
            Some(_) => false,
            None => {
                self.paused_since.set(Some(now));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STUCK: Seen = Seen { sound_playing: true, picture_paused: true, past_picture_end: false };
    const FINE: Seen = Seen { sound_playing: true, picture_paused: false, past_picture_end: false };

    /// The reported bug, as the transport sees it: Play pressed the moment the clip is ready, the
    /// sound clock runs from 68.0 s, and the video player stays Paused (the prime's Pause landed
    /// after Play). Before the fix nothing but a drift re-seek every 500 ms touched the picture,
    /// so it moved one frame per seek for as long as the clip played.
    #[test]
    fn play_right_after_load_with_a_late_pause_replays_the_picture() {
        let t0 = Instant::now();
        let guard = PictureGuard::default();
        // The editor's play(): the guard starts fresh.
        guard.reset();
        let mut plays = Vec::new();
        // One clock read per 60 Hz frame for 3 s: sound running, picture paused throughout.
        for frame in 0..180u64 {
            let now = t0 + Duration::from_millis(frame * 1000 / 60);
            if guard.observe(STUCK, now) {
                plays.push(now.duration_since(t0));
            }
        }
        let first = plays.first().copied().expect("a paused picture under running sound must get Play again");
        assert!(first >= PAUSED_FOR && first < PAUSED_FOR + Duration::from_millis(20), "first Play at {first:?}");
        // While it stays stuck, Play is sent again once per grace time, never every frame.
        assert!(plays.len() >= 6 && plays.len() <= 8, "{} Plays in 3 s", plays.len());
    }

    #[test]
    fn a_picture_that_starts_playing_is_left_alone() {
        let t0 = Instant::now();
        let guard = PictureGuard::default();
        // A normal start: Paused for a few frames while Play lands, then Playing.
        for frame in 0..15u64 {
            assert!(!guard.observe(STUCK, t0 + Duration::from_millis(frame * 16)));
        }
        for frame in 15..300u64 {
            assert!(!guard.observe(FINE, t0 + Duration::from_millis(frame * 16)));
        }
        // A short paused read later (a seek while playing) does not trigger either.
        let later = t0 + Duration::from_secs(10);
        assert!(!guard.observe(STUCK, later));
        assert!(!guard.observe(STUCK, later + Duration::from_millis(300)));
        assert!(!guard.observe(FINE, later + Duration::from_millis(320)));
        assert!(!guard.observe(STUCK, later + Duration::from_millis(500)), "the timer started over");
    }

    #[test]
    fn a_paused_picture_is_right_when_the_sound_is_paused_or_past_the_last_frame() {
        let t0 = Instant::now();
        let guard = PictureGuard::default();
        let paused = Seen { sound_playing: false, ..STUCK };
        let tail = Seen { past_picture_end: true, ..STUCK };
        for step in 0..20u64 {
            let now = t0 + Duration::from_millis(step * 100);
            assert!(!guard.observe(paused, now));
            assert!(!guard.observe(tail, now));
        }
    }

    #[test]
    fn reset_starts_the_grace_time_again() {
        let t0 = Instant::now();
        let guard = PictureGuard::default();
        guard.observe(STUCK, t0);
        guard.reset();
        assert!(!guard.observe(STUCK, t0 + PAUSED_FOR));
        assert!(guard.observe(STUCK, t0 + PAUSED_FOR * 2));
    }
}
