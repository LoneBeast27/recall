//! What a click on a clip tile does. On the picture a plain click opens the clip at once. On the
//! title a plain click opens it once the system double-click time passes with no second click,
//! and a double-click there renames it instead: the first click of that double-click never opens
//! the editor. Shift and Ctrl clicks only change the selection (`library_select.rs`).
use std::time::{Duration, Instant};

use gpui::{ClickEvent, Context, Window};

use crate::root::{Root, Route};

impl Root {
    /// A click on the tile's picture or meta line.
    pub(crate) fn tile_body_click(&mut self, path: &str, event: &ClickEvent) {
        if self.press_dismisses {
            return;
        }
        let m = event.modifiers();
        self.lib.click(path, m.control, m.shift);
        if !m.control && !m.shift {
            self.open_clip(path.to_owned());
            // The second press of a double-click lands on the editor: `on_press` swallows it.
            self.opened_by_click = true;
        }
    }

    /// A click on the tile's title.
    pub(crate) fn tile_title_click(&mut self, path: &str, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.press_dismisses {
            return;
        }
        let m = event.modifiers();
        if m.control || m.shift {
            self.lib.click(path, m.control, m.shift);
            return;
        }
        match event.click_count() {
            1 => {
                self.lib.click(path, false, false);
                let at = Instant::now();
                self.lib.title_click = Some((path.to_owned(), at));
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(double_click_time()).await;
                    let _ = this.update(cx, |this, cx| {
                        // Any later press (`on_press`) or a page change ends the wait.
                        let Some((path, _)) = this.lib.title_click.take_if(|(_, t)| *t == at) else { return };
                        if this.route == Route::Library && this.lib.record(&path).is_some() {
                            this.open_clip(path);
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            2 => {
                self.lib.title_click = None;
                self.begin_rename(path.to_owned(), window);
            }
            _ => {}
        }
    }
}

/// The system double-click time (Control Panel → Mouse), 500 ms by default.
fn double_click_time() -> Duration {
    let ms = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() };
    Duration::from_millis(if ms == 0 { 500 } else { u64::from(ms) })
}
