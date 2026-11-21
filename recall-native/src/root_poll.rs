//! The UI pass: applies what changed (worker results, the recorder's snapshot, timers) and
//! draws when something visible did. It runs when woken or at its next deadline
//! (`root_wake.rs`), no longer every 30 ms in front and 250 ms behind. In the background a
//! recorder status change is applied but not drawn; the view draws once when Recall comes back
//! to the front.
//! Measured 2026-11-04: the open library cost 1.95% of a core on its main thread while a
//! game was recording, most of it the old fixed poll and the once-a-second status redraw.
use std::time::Instant;

use gpui::{Context, Entity, Timer, Window};

use super::{Root, Route, ToastKind};
use crate::services::ui_wake;

impl Root {
    pub fn start_polling(entity: Entity<Self>, window: &mut Window, cx: &mut gpui::App) {
        // Clock changes and wakes from sleep reach the library's day labels (`roll_day`); the
        // listener thread sleeps in the OS until Windows broadcasts one.
        let _ = crate::services::os_events::listening();
        // Front/back comes from the window's activation events, not a check per pass.
        entity.update(cx, |this, cx| {
            this.active = window.is_window_active();
            this.activation = Some(cx.observe_window_activation(window, |this, window, _| {
                this.active = window.is_window_active();
                ui_wake::wake();
            }));
        });
        window
            .spawn(cx, async move |cx| {
                loop {
                    let Ok(next) = entity.update(cx, |this, cx| {
                        this.poll(cx);
                        this.next_pass()
                    }) else {
                        break;
                    };
                    PASSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    ui_wake::wait(next.map(Timer::after)).await;
                }
            })
            .detach();
    }

    /// Another window is in front or Recall is minimized (the window is not active), and no
    /// preview is playing. Scripted instances (offscreen or background) never count as
    /// background: their checks read every frame.
    pub(super) fn in_background(&self) -> bool {
        if self.route == Route::Editor && self.ed.playing {
            return false;
        }
        if self.hwnd.is_none() || crate::offscreen::enabled() || std::env::var_os("RECALL_NATIVE_BACKGROUND").is_some() {
            return false;
        }
        !self.active
    }

    /// A recorder status change: drawn now in front, or once Recall is back in front.
    pub(super) fn status_changed(&mut self) -> bool {
        if self.in_background() {
            self.status_stale = true;
            return false;
        }
        true
    }

    /// The pill changes with no new pipe line: the recorder stopped answering, or a
    /// drain countdown reached its next second.
    pub(super) fn recorder_clock_tick(&mut self) -> bool {
        let unresponsive = self.daemon.unresponsive();
        let draining = self.snap.session.as_ref().filter(|s| s.state == "draining");
        let left = draining.and_then(|s| s.drain_left(crate::services::daemon::unix_ms(), std::time::Instant::now()));
        if unresponsive == self.recorder_unresponsive && left == self.drain_drawn {
            return false;
        }
        if unresponsive != self.recorder_unresponsive {
            eprintln!("Recall native: recorder {}", if unresponsive { "not responding" } else { "responding again" });
            crate::trace::note(if unresponsive { "recorder not responding" } else { "recorder responding again" });
        }
        self.recorder_unresponsive = unresponsive;
        self.drain_drawn = left;
        self.status_changed()
    }

    /// Recall came back to the front with a status change it has not drawn yet.
    pub(super) fn back_in_front(&mut self) -> bool {
        if self.status_stale && !self.in_background() {
            self.status_stale = false;
            return true;
        }
        false
    }

    pub(super) fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = self.back_in_front();
        while let Ok(done) = self.workers.done.try_recv() {
            changed = true;
            self.on_done(done);
        }
        changed |= self.poll_daemon();
        changed |= self.recorder_clock_tick();
        changed |= self.reach_tick();
        // Once the library is listed, the window is in front (not minimised, hidden at login or
        // behind a game) and no game is captured: the first real clip then skips Media
        // Foundation's one-time start (~200 ms). The warm-up player closes at its first frame.
        if !self.player_warmed && !self.lib.loading && !self.in_background() {
            self.player_warmed = self.ed.warm_player(self.recorder_state().game);
        }
        changed |= self.ed.tick(self.route == Route::Editor, &self.workers);
        crate::trace::record(self.route.name(), &self.ed, self.lib.records.len());
        crate::trace::dump_layout(self.route.name(), self.lib.records.len());
        if let Some(error) = self.ed.last_error.take() {
            self.show_toast(ToastKind::Bad, "Preview", Some(error));
            changed = true;
        }
        changed |= self.lib.fit_columns();
        // Past midnight "Today" becomes "Yesterday" without a new listing.
        changed |= self.lib.roll_day();
        if self.route == Route::Library {
            self.lib.queue_visible(&self.workers);
            self.lib.note_scroll_depth();
        }
        // A thumbnail rewritten larger in place: drop gpui's decode so the tile reloads the file.
        for thumb in self.lib.refreshed_images.drain(..) {
            gpui::ImageSource::from(std::path::PathBuf::from(thumb)).remove_asset(cx);
            changed = true;
        }
        self.lib.flush_meta_if_due();
        self.lib.ssd.poll(&self.workers, !self.lib.loading);
        if let Some(notice) = self.lib.notice.take() {
            self.show_toast(ToastKind::Bad, notice, None);
            changed = true;
        }
        // The tile with the rename field left the screen: draw again so `settle_focus` sees it.
        changed |= self.lib.renaming.is_some() && !self.rename_painted.get();
        // New meter levels or MIDI messages on the Audio page.
        changed |= self.audio_moved();
        // A fix or Undo finished on its own thread: the notice shows the result.
        changed |= crate::fix_offers::take_changed();
        // Same for the cache size field when Settings → Storage closes while it's open.
        changed |= self.cache.editing && !self.cache.painted.get();
        changed |= self.poll_timers();
        // The update bar animates only in front (`UpdateUi::set_front`).
        self.update.set_front(!self.in_background());
        changed |= self.update.tick();
        // Setup has the update (it quits the recorder, saving first): this app closes itself.
        if self.update.take_quit() {
            self.flush_on_close();
            cx.quit();
            return;
        }
        // The old-engine notice's Reinstall (`repair::reinstall_from_notice`): Setup has it, so
        // this app closes as for an update; else the user reads why.
        match crate::repair::take_outcome() {
            Some(Ok(())) => {
                self.flush_on_close();
                cx.quit();
                return;
            }
            Some(Err(why)) => {
                self.show_toast(ToastKind::Bad, format!("Reinstall didn't start: {why}"), None);
                changed = true;
            }
            None => {}
        }
        if let Some((good, notice)) = self.update.notice.take() {
            self.show_toast(if good { ToastKind::Good } else { ToastKind::Bad }, notice, None);
        }
        if changed {
            cx.notify();
        }
    }

    /// A new recorder snapshot: apply it, route its events. True when the status changed.
    fn poll_daemon(&mut self) -> bool {
        if self.daemon.revision() == self.daemon_rev {
            return false;
        }
        self.daemon_rev = self.daemon.revision();
        self.snap = self.daemon.snapshot();
        self.resolve_setup_resume();
        // Background cache filling pauses while a game is being recorded.
        let busy = self.snap.session.as_ref().is_some_and(|s| s.capturing());
        crate::services::storage::set_recorder_busy(busy);
        crate::services::storage::set_recorder_connected(self.snap.connected);
        self.sync_recordings_folder();
        for event in self.daemon.drain_events() {
            self.on_daemon_event(event);
        }
        self.status_changed()
    }

    pub(crate) fn finish_setup_recording(&mut self, record_after: bool) {
        // The recorder Setup started stays paused only while Setup runs: finishing resumes it
        // ("Start recorder") or hands the pause to the user, so its owner's exit cannot resume it.
        self.setup_resume_pending = Some(record_after);
        if !record_after {
            self.controller_check_at = None;
        }
        self.resolve_setup_resume();
    }

    fn resolve_setup_resume(&mut self) {
        let Some(record_after) = self.setup_resume_pending.filter(|_| self.snap.connected) else { return };
        let Some(session) = self.snap.session.as_ref() else { return };
        self.setup_resume_pending = None;
        if let Some(cmd) = setup_finish_command(session.setup_owner, std::process::id(), record_after) {
            self.send(cmd);
        }
    }

    /// Time-outs: the storage-limit dry run, the recorder's first start, the toast. True when
    /// something changed on screen.
    fn poll_timers(&mut self) -> bool {
        let mut changed = false;
        // A recorder that never answers the dry run (an older build, a dropped pipe) sets nothing.
        if self.storage_asking.is_some_and(|(_, at)| at.elapsed() > crate::views::storage_limit::ASK_TIMEOUT) {
            self.storage_asking = None;
            self.show_toast(ToastKind::Bad, "Recorder didn't answer", Some("Storage limit not changed".to_owned()));
            changed = true;
        }
        // Opening Recall means recording: start the recorder once if it isn't running.
        if self.controller_check_at.is_some_and(|at| Instant::now() >= at) {
            self.controller_check_at = None;
            if !self.snap.connected && self.setup.is_some() {
                self.start_setup_recorder();
                changed = true;
            } else if !self.snap.connected {
                if let Err(error) = crate::services::daemon::start_controller() {
                    self.show_toast(ToastKind::Bad, "Couldn't start the recorder", Some(error));
                    changed = true;
                }
            }
        }
        if self.toast.as_ref().is_some_and(|t| Instant::now() >= t.until) {
            self.toast = None;
            changed = true;
        }
        changed
    }
}

/// UI passes run since start (the before/after wake count: `RECALL_NATIVE_PASSES` prints it).
pub(crate) static PASSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What a finished Setup tells the recorder it started (`setup_owner` = this app's pid):
/// resume it, or keep it paused as the user's own pause. A recorder this Setup does not own
/// is left alone.
pub(crate) fn setup_finish_command(owner: Option<u32>, me: u32, record_after: bool) -> Option<crate::services::daemon::DaemonCommand> {
    use crate::services::daemon::DaemonCommand;
    (owner == Some(me)).then_some(if record_after { DaemonCommand::Resume } else { DaemonCommand::Pause })
}

#[cfg(test)]
mod setup_finish_tests {
    use super::setup_finish_command;
    use crate::services::daemon::DaemonCommand;

    #[test]
    fn finishing_setup_releases_the_recorder_it_paused() {
        assert_eq!(setup_finish_command(Some(7), 7, true), Some(DaemonCommand::Resume));
        assert_eq!(setup_finish_command(Some(7), 7, false), Some(DaemonCommand::Pause), "kept paused, owned by the user now");
        assert_eq!(setup_finish_command(Some(8), 7, true), None, "another app's recorder");
        assert_eq!(setup_finish_command(None, 7, true), None, "nothing owned");
    }
}
