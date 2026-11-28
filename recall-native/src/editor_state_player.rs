//! The editor's native player: where its window sits, and making it again after a failure.
//!
//! A player that failed is made again, as an app restart did. The player (`NativePreview`) is
//! made once at start and lives as long as the app, so whatever broke in it (a sound worker that
//! ended or stopped answering, a Media Foundation player that kept failing) stayed broken for
//! every later clip until Recall was restarted (Rohan's report, 2026-11-29).
use super::{EditorState, same_path};
use crate::media::NativePreview;

/// What the editor knows about its player's health.
#[derive(Default)]
pub(crate) struct Renew {
    /// An open or a refresh failed on this player since it was made.
    broken: bool,
    /// The open (its generation) whose clip was already given a fresh player once.
    retried: Option<u64>,
}

/// The next clip gets a fresh player: this one failed, or its sound worker stopped answering.
fn renew_due(broken: bool, needs_fresh: bool) -> bool {
    broken || needs_fresh
}

/// A clip whose player failed gets one fresh player before a preview copy is made: once per
/// open, and never for a clip still loading (its copy and decoder go with the old player).
fn retry_due(retried: Option<u64>, generation: u64, streaming: bool) -> bool {
    retried != Some(generation) && !streaming
}

impl EditorState {
    /// Moves the native video to the layout's preview rect; true when it moved (repaint so the
    /// backdrop's cut-out follows).
    pub(crate) fn apply_preview_rect(&mut self, visible: bool) -> bool {
        let next = if visible && self.info.is_some() && !self.preview_failed { self.preview_rect.get().unwrap_or((0, 0, 0, 0)) } else { (0, 0, 0, 0) };
        if next == self.applied_rect {
            return false;
        }
        self.applied_rect = next;
        if let Some(preview) = self.preview.as_mut() {
            let shown = next.2 > 0 && next.3 > 0;
            let result = if shown { preview.rect(next.0, next.1, next.2, next.3) } else { preview.hide() };
            match result {
                Ok(()) => self.hole.set(shown.then_some(next)),
                Err(error) => {
                    self.hole.set(None);
                    self.status = error;
                }
            }
        }
        true
    }

    pub fn hide_preview(&mut self) {
        self.hole.set(None);
        self.applied_rect = (0, 0, 0, 0);
        if let Some(preview) = self.preview.as_mut() {
            let _ = preview.hide();
        }
    }

    /// An open, a swap or a refresh failed on the current player.
    pub(super) fn player_failed(&mut self) {
        self.renew.broken = true;
    }

    /// Before a new clip opens: a player that failed, or whose sound stopped answering, is
    /// replaced, so this clip does not inherit what went wrong with the last one.
    pub(super) fn renew_if_broken(&mut self) {
        let stuck = self.preview.as_ref().is_some_and(NativePreview::needs_fresh);
        if renew_due(self.renew.broken, stuck) {
            self.renew_preview(if stuck { "the sound worker stopped answering" } else { "the last clip's player failed" });
        }
    }

    /// Swaps in a fresh player; the old one is let go after it exists. False when there is no
    /// player to replace or a new one could not be made (the old one stays).
    fn renew_preview(&mut self, why: &str) -> bool {
        let Some(old) = self.preview.as_ref() else { return false };
        match old.fresh() {
            Ok(fresh) => {
                drop(self.preview.replace(fresh));
                self.renew.broken = false;
                self.applied_rect = (0, 0, 0, 0);
                crate::trace::player_event("player made again", serde_json::json!({ "why": why }));
                true
            }
            Err(error) => {
                crate::trace::player_event("player could not be made again", serde_json::json!({ "why": why, "error": error }));
                false
            }
        }
    }

    /// The open clip's player failed: open it once more in a fresh player, at the same spot.
    /// True when that open started (its result shows on a later tick); false to carry on with
    /// the usual fallbacks (a preview copy).
    pub(super) fn retry_in_fresh_player(&mut self) -> bool {
        let streaming = self.preview.as_ref().is_some_and(|preview| preview.stream().is_some());
        if !retry_due(self.renew.retried, self.generation, streaming) {
            return false;
        }
        self.renew.retried = Some(self.generation);
        let Some(info) = self.info.clone() else { return false };
        let at = self.position();
        let path = self.playback_path.clone().filter(|path| std::path::Path::new(path).is_file()).unwrap_or_else(|| info.path.clone());
        if !self.renew_preview("the open clip's player failed") {
            return false;
        }
        let limits = if same_path(&path, &info.path) { self.limits } else { crate::services::library::video_limits(&path, info.duration) };
        let tracks = self.preview_tracks(&info);
        let Some(preview) = self.preview.as_mut() else { return false };
        match preview.open(&path, &tracks, info.duration, limits) {
            Ok(()) => {
                self.playback_path = Some(path);
                self.pending_seek = Some(at);
                self.preview_failed = false;
                self.status = "Loading preview…".into();
                true
            }
            Err(error) => {
                self.player_failed();
                self.last_error = Some(error);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{renew_due, retry_due};

    #[test]
    fn the_next_clip_gets_a_fresh_player_after_a_failure_or_a_stuck_sound_worker() {
        assert!(!renew_due(false, false), "a healthy player is kept: no extra start-up cost");
        assert!(renew_due(true, false), "the last open or refresh failed");
        assert!(renew_due(false, true), "the sound worker ended or stopped answering");
    }

    #[test]
    fn a_failed_clip_is_retried_in_a_fresh_player_once_per_open() {
        assert!(retry_due(None, 7, false), "first failure of this open");
        assert!(!retry_due(Some(7), 7, false), "the fresh player failed too: make a preview copy");
        assert!(retry_due(Some(6), 7, false), "a new open of the clip may retry again");
        assert!(!retry_due(None, 7, true), "a clip still loading keeps its copy: no retry");
    }
}
