//! The UI poll: every 30 ms while Recall is in front, every 250 ms while another window is
//! (a game, usually) or Recall is minimized. In the background a recorder status change is
//! applied but not drawn; the view draws once when Recall comes back to the front.
//! Measured 2026-11-04: the open library cost 1.95% of a core on its main thread while a
//! game was recording, most of it this poll and the once-a-second status redraw.
use std::time::{Duration, Instant};

use gpui::{Context, Entity, Timer, Window};

use super::{Root, Route, ToastKind};

const FRONT: Duration = Duration::from_millis(30);
const BACK: Duration = Duration::from_millis(250);

impl Root {
    pub fn start_polling(entity: Entity<Self>, window: &mut Window, cx: &mut gpui::App) {
        window
            .spawn(cx, async move |cx| {
                let mut every = FRONT;
                loop {
                    Timer::after(every).await;
                    match entity.update(cx, |this, cx| {
                        this.poll(cx);
                        if this.in_background() { BACK } else { FRONT }
                    }) {
                        Ok(next) => every = next,
                        Err(_) => break,
                    }
                }
            })
            .detach();
    }

    /// Another window is in front or Recall is minimized, and no preview is playing.
    /// Scripted instances (offscreen or background) never count as background: their checks
    /// read every frame.
    pub(super) fn in_background(&self) -> bool {
        use windows::Win32::{
            Foundation::HWND,
            UI::WindowsAndMessaging::{GetForegroundWindow, IsIconic},
        };
        if self.route == Route::Editor && self.ed.playing {
            return false;
        }
        let Some(raw) = self.hwnd else { return false };
        if crate::offscreen::enabled() || std::env::var_os("RECALL_NATIVE_BACKGROUND").is_some() {
            return false;
        }
        let hwnd = HWND(raw as *mut _);
        unsafe { IsIconic(hwnd).as_bool() || GetForegroundWindow() != hwnd }
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
        let drain = self.snap.session.as_ref().filter(|s| s.state == "draining").and_then(|s| s.drain.as_ref());
        let left = drain.map(|d| d.remaining_secs(crate::services::daemon::unix_ms()));
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
        changed |= self.ed.tick(self.route == Route::Editor, &self.workers);
        crate::trace::record(self.route.name(), &self.ed, self.lib.records.len());
        crate::trace::dump_layout(self.route.name(), self.lib.records.len());
        if let Some(error) = self.ed.last_error.take() {
            self.show_toast(ToastKind::Bad, "Preview", Some(error));
            changed = true;
        }
        changed |= self.lib.fit_columns();
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
        // Same for the cache size field when Settings → Storage closes while it's open.
        changed |= self.cache.editing && !self.cache.painted.get();
        changed |= self.poll_timers();
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
            if !self.snap.connected {
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
