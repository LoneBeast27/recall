//! Full screen. F11 fills the monitor with the whole window (title bar and all). In the editor
//! the preview's full screen button (or F) fills it with the video and its transport alone;
//! leaving that returns to how the window was before.
//!
//! GPUI's `toggle_fullscreen` flips the window on its executor, so `is_fullscreen` lags a frame:
//! the mode here is the truth and each change toggles the window only when it must.
use gpui::Window;

use super::{Root, Route};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FullScreen {
    #[default]
    Off,
    /// F11: the whole app fills the monitor.
    Window,
    /// The editor's video fills the monitor; `from_window` = F11 was on before.
    Video { from_window: bool },
}

impl FullScreen {
    pub fn video(self) -> bool {
        matches!(self, FullScreen::Video { .. })
    }

    fn window_full(self) -> bool {
        self != FullScreen::Off
    }

    /// F11: the window on or off; from the video it leaves full screen entirely.
    pub fn after_f11(self) -> FullScreen {
        match self {
            FullScreen::Off => FullScreen::Window,
            _ => FullScreen::Off,
        }
    }

    /// The preview's button (or F): into the video, or back to how the window was.
    pub fn after_video_toggle(self) -> FullScreen {
        match self {
            FullScreen::Off => FullScreen::Video { from_window: false },
            FullScreen::Window => FullScreen::Video { from_window: true },
            FullScreen::Video { from_window } => FullScreen::left_video(from_window),
        }
    }

    fn left_video(from_window: bool) -> FullScreen {
        if from_window { FullScreen::Window } else { FullScreen::Off }
    }

    /// Escape or leaving the editor: out of the video, back to how the window was.
    pub fn after_video_exit(self) -> FullScreen {
        match self {
            FullScreen::Video { from_window } => FullScreen::left_video(from_window),
            other => other,
        }
    }
}

/// Scripted layout checks (`RECALL_NATIVE_OFFSCREEN`) may start in the video full screen with
/// `RECALL_NATIVE_FULL=video`: the layout only; the off-screen window keeps its size and place.
pub fn start_mode() -> FullScreen {
    let video = crate::offscreen::enabled() && std::env::var("RECALL_NATIVE_FULL").as_deref() == Ok("video");
    if video { FullScreen::Video { from_window: false } } else { FullScreen::Off }
}

impl Root {
    fn set_full(&mut self, next: FullScreen, window: &mut Window) {
        // A scripted check's off-screen window never moves onto a monitor.
        if next.window_full() != self.full.window_full() && !crate::offscreen::enabled() {
            window.toggle_fullscreen();
        }
        self.full = next;
        // The video moves to its new place on the next frame (`schedule_frames`).
        window.refresh();
    }

    pub fn toggle_window_full(&mut self, window: &mut Window) {
        self.set_full(self.full.after_f11(), window);
    }

    pub fn toggle_video_full(&mut self, window: &mut Window) {
        self.set_full(self.full.after_video_toggle(), window);
    }

    /// Out of the video full screen; returns false when it was not on.
    pub fn exit_video_full(&mut self, window: &mut Window) -> bool {
        if !self.full.video() {
            return false;
        }
        self.set_full(self.full.after_video_exit(), window);
        true
    }

    /// The video full screen belongs to the editor: any other page leaves it.
    pub fn settle_full(&mut self, window: &mut Window) {
        if self.route != Route::Editor {
            self.exit_video_full(window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FullScreen::{self, *};

    #[test]
    fn f11_toggles_the_window_and_leaves_the_video() {
        assert_eq!(Off.after_f11(), Window);
        assert_eq!(Window.after_f11(), Off);
        assert_eq!(Video { from_window: false }.after_f11(), Off);
        assert_eq!(Video { from_window: true }.after_f11(), Off);
    }

    #[test]
    fn the_video_returns_to_how_the_window_was() {
        let from_off = Off.after_video_toggle();
        assert_eq!(from_off, Video { from_window: false });
        assert_eq!(from_off.after_video_toggle(), Off);
        assert_eq!(from_off.after_video_exit(), Off);
        let from_window = Window.after_video_toggle();
        assert_eq!(from_window, Video { from_window: true });
        assert_eq!(from_window.after_video_toggle(), Window);
        assert_eq!(from_window.after_video_exit(), Window);
    }

    #[test]
    fn exit_without_the_video_changes_nothing() {
        assert_eq!(Off.after_video_exit(), Off);
        assert_eq!(Window.after_video_exit(), Window);
        assert!(!FullScreen::default().video());
    }
}
