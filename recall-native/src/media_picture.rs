//! A picture opened ahead of its clip's details (`open_picture`), and the one-time warm-up
//! of the player at start (`warm_up`): Media Foundation's first player in a process costs
//! ~150-180 ms more than the next ones (measured 2026-11-21, 377 vs 185-241 ms to a picture).
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;

use super::{NativePreview, PreviewTrack, normalize_windows_path, wide};

/// A warm-up that has not reached its first frame by then is given up.
const WARM_UP_LIMIT: Duration = Duration::from_secs(5);

impl NativePreview {
    /// Starts the player on a clip's SSD copy before its details (stems, edits, limits) are in:
    /// Media Foundation's setup (~0.25 s) then runs beside the rest of the open. [`Self::open`]
    /// with the same path takes this player over; anything else replaces it. Muted and hidden
    /// until then, and a failure here is only reported when the clip opens.
    #[allow(dead_code)] // the media examples include this file without the editor
    pub fn open_picture(&mut self, path: &str) -> Result<(), String> {
        if path.is_empty() {
            return Err("preview video path is empty".into());
        }
        let (player, callback) = self.new_player((None, None))?;
        let url = wide(&normalize_windows_path(path));
        if let Err(error) = unsafe { player.CreateMediaItemFromURL(PCWSTR(url.as_ptr()), false, 0, None) } {
            let _ = unsafe { player.Shutdown() };
            return Err(format!("could not open native video preview: {error}"));
        }
        let _ = unsafe { player.SetMute(true) };
        self.keep_player(player, callback);
        self.primed = Some(path.to_owned());
        Ok(())
    }

    /// The player [`Self::open_picture`] started is for `path` and still healthy. Clears the mark
    /// either way: a picture is claimed once.
    pub(super) fn claim_picture(&mut self, path: &str) -> bool {
        let primed = self.primed.take();
        let failed = std::mem::take(&mut self.prime_failed);
        claimable(primed.as_deref(), failed, path, self.player.is_some() && self.stream.is_none() && self.error().is_none())
    }

    /// The stems of a clip whose picture is already open; without them the player is shut, as
    /// [`Self::open`] does when they fail.
    pub(super) fn open_sound(&mut self, audio: &[PreviewTrack], duration: f64) -> Result<(), String> {
        self.has_audio = !audio.is_empty();
        if self.has_audio {
            if let Err(error) = self.audio.open(audio, duration) {
                let _ = self.clear_clip();
                return Err(error);
            }
            self.sound.opened(Some(super::sound::SoundSpec::Stems(audio.to_vec(), duration)));
        }
        Ok(())
    }

    /// Opens `path` (a tiny clip the app ships) as a hidden, muted picture and lets it go at
    /// its first frame: the decoder and the rest of Media Foundation are loaded for the first
    /// real clip, and nothing stays open. A clip opened meanwhile simply replaces it.
    #[allow(dead_code)] // the media examples include this file without the editor
    /// `marker` was written just before and is removed when the warm-up player is let go, so a
    /// run that never got there (a crash in the decoder) leaves it for the next start to see.
    pub fn warm_up(&mut self, path: &str, marker: &std::path::Path) -> Result<(), String> {
        if let Err(error) = self.open_picture(path) {
            let _ = std::fs::remove_file(marker);
            return Err(error);
        }
        self.warming = Some((Instant::now(), marker.to_path_buf()));
        Ok(())
    }

    /// The app is closing normally: a warm-up still running did not go down, so its marker goes
    /// (only a crash leaves it). The player itself is shut with the rest at drop.
    pub fn closing(&mut self) {
        if let Some((_, marker)) = self.warming.as_ref() {
            let _ = std::fs::remove_file(marker);
        }
    }

    /// Whether the warm-up player is still open (tests and the trace).
    #[allow(dead_code)] // the media examples include this file without the editor
    pub fn warming(&self) -> bool {
        self.warming.is_some()
    }

    pub fn refresh(&mut self) -> Result<bool, String> {
        let result = self.refresh_player();
        if self.primed.is_none() {
            return result;
        }
        if let Some(started) = self.warming.as_ref().map(|(started, _)| *started) {
            let shown = self.window_state.presentable.load(Ordering::Acquire);
            if warm_up_over(shown, result.is_err() || self.prime_failed, started.elapsed()) {
                let _ = self.clear_clip();
                return Ok(false);
            }
        }
        // A picture opened ahead of its clip: a failure waits for the clip's own open, which
        // starts a new player and reports it then.
        self.prime_failed |= result.is_err();
        Ok(result.unwrap_or(false))
    }

}

/// A picture opened ahead for `primed` serves an open of `path` only when it is the same file,
/// it did not fail, and its player is still there and well (`healthy`).
fn claimable(primed: Option<&str>, failed: bool, path: &str, healthy: bool) -> bool {
    primed == Some(path) && !failed && healthy
}

/// The warm-up is let go at its first frame, on a failure, or after [`WARM_UP_LIMIT`].
fn warm_up_over(shown: bool, failed: bool, elapsed: Duration) -> bool {
    shown || failed || elapsed >= WARM_UP_LIMIT
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{claimable, warm_up_over};

    #[test]
    fn only_a_healthy_picture_of_the_same_copy_is_taken_over() {
        let copy = r"C:\cache\video\0123456789abcdef.mp4";
        assert!(claimable(Some(copy), false, copy, true));
        assert!(!claimable(None, false, copy, true), "nothing was opened ahead");
        assert!(!claimable(Some(copy), false, r"V:\clips\made-up.mp4", true), "the open plays another file (the copy went)");
        assert!(!claimable(Some(copy), true, copy, true), "the early player failed: a new one reports why");
        assert!(!claimable(Some(copy), false, copy, false), "the player is gone or in error");
    }

    #[test]
    fn the_warm_up_lets_go_at_its_first_frame_a_failure_or_the_limit() {
        assert!(!warm_up_over(false, false, Duration::from_millis(300)), "still loading");
        assert!(warm_up_over(true, false, Duration::from_millis(300)), "first frame: done");
        assert!(warm_up_over(false, true, Duration::from_millis(10)), "failed: nothing kept");
        assert!(warm_up_over(false, false, Duration::from_secs(5)), "never kept past the limit");
    }
}
