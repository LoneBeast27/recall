//! The UI poll: every 30 ms while Recall is in front, every 250 ms while another window is
//! (a game, usually) or Recall is minimized. In the background a recorder status change is
//! applied but not drawn; the view draws once when Recall comes back to the front.
//! Measured 2026-11-04: the open library cost 1.95% of a core on its main thread while a
//! game was recording, most of it this poll and the once-a-second status redraw.
use std::time::Duration;

use gpui::{Entity, Timer, Window};

use super::{Root, Route};

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
}
