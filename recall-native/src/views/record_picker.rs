//! The Record picker (maintainer, 2026-11-28: "when i click on the record button, let it show a
//! utility that asks you what to capture"): with 2 or more monitors, Record asks for an app
//! window or a whole screen first. Two tabs, Applications and Entire Screen, of cards in two
//! columns; a window card shows a live preview Windows draws itself (`services::dwm_thumbs`),
//! a screen card the monitor layout with that monitor lit. The bottom row says what size and
//! rate the recording takes (Settings › Recording, the gear) and offers "Don't ask again"
//! (`recording.ask_record_target`, back on from Settings › Recording).
//!
//! Tab switches tabs, Enter starts, Esc closes.
//!
//! It also opens from the Record shortcut when no game is followed (the recorder brings this
//! window up: `record_picker` event, or `RECALL_RECORD_PICKER` on a window it launches). The
//! lists are read once when it opens, off the UI thread; nothing runs while it is closed.
use std::path::PathBuf;

use gpui::{AnyElement, Context, FontWeight, Window, canvas, div, prelude::*, px};
use serde_json::{Value, json};

use crate::{
    root::Root,
    services::{
        capture_targets::{self, Choice, LastPick, MonitorCard, Tab, WindowCard},
        capture_targets_win,
        daemon::DaemonCommand,
    },
    theme::{self, c, ca},
    ui::{self, Icon, Variant, icon},
};

pub const TITLE: &str = "What do you want to record?";
pub const START: &str = "Start recording";
pub const DONT_ASK: &str = "Don't ask again";
/// The env var a window the recorder launches for the shortcut reads (`record_tray::PICKER_ENV`).
pub const PICKER_ENV: &str = "RECALL_RECORD_PICKER";

/// The picker while it is open.
#[derive(Clone, Debug, Default)]
pub struct Picker {
    pub tab: Tab,
    /// `None` while the lists are read.
    pub windows: Option<Vec<WindowCard>>,
    pub monitors: Vec<MonitorCard>,
    pub chosen: Option<Choice>,
    pub dont_ask: bool,
}

/// Whether Record asks first: the setting is on (a recorder too old to know it never asks)
/// and 2 or more monitors are connected.
pub fn asks(setting: Option<bool>, monitors: usize) -> bool {
    setting == Some(true) && monitors >= 2
}

/// `%APPDATA%\Recall\record-pick.json` (a test's own folder under test: `user_dirs::checked`).
fn last_pick_path() -> Option<PathBuf> {
    crate::user_dirs::appdata().map(|root| crate::user_dirs::checked(root.join("Recall").join("record-pick.json")))
}

fn load_last_pick() -> Option<LastPick> {
    std::fs::read(last_pick_path()?).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

fn save_last_pick(pick: &LastPick) {
    let Some(path) = last_pick_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(pick)
        && let Err(e) = crate::services::durable::write_durable(&path, &bytes)
    {
        eprintln!("record picker: last pick not saved ({e})");
    }
}

impl Root {
    /// Title-bar Record: the picker when it [`asks`], else `false` (the plain toggle).
    pub(crate) fn record_click_opens_picker(&mut self, cx: &mut Context<Self>) -> bool {
        let setting = self.config_value("recording", "ask_record_target").and_then(Value::as_bool);
        if !asks(setting, capture_targets_win::monitor_count()) {
            return false;
        }
        self.open_record_picker(cx);
        true
    }

    /// Open the picker and read its lists in the background.
    pub(crate) fn open_record_picker(&mut self, cx: &mut Context<Self>) {
        if self.record_picker.is_some() {
            return;
        }
        self.record_picker = Some(Picker::default());
        let followed = self.snap.session.as_ref().and_then(|s| s.target.as_ref()).map(|t| t.hwnd as isize).filter(|h| *h != 0);
        let known: Vec<String> = self.target_icons.keys().cloned().collect();
        cx.spawn(async move |this, cx| {
            let (windows, monitors, icons, last) = cx
                .background_executor()
                .spawn(async move {
                    let windows = capture_targets_win::windows_blocking();
                    let monitors = capture_targets_win::monitors_blocking();
                    let mut icons = Vec::new();
                    for w in windows.iter().filter(|w| !w.path.is_empty()) {
                        let key = w.exe.to_lowercase();
                        if !known.contains(&key) && !icons.iter().any(|(k, _)| *k == key)
                            && let Some(image) = crate::services::app_icons::icon_for(std::path::Path::new(&w.path))
                        {
                            icons.push((key, image));
                        }
                    }
                    (windows, monitors, icons, load_last_pick())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.target_icons.extend(icons);
                let Some(picker) = this.record_picker.as_mut() else { return };
                let windows = capture_targets::order_windows(windows, followed);
                let (tab, chosen) = capture_targets::preselect(&windows, &monitors, last.as_ref(), followed);
                picker.tab = tab;
                picker.chosen = chosen;
                picker.windows = Some(windows);
                picker.monitors = monitors;
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn close_record_picker(&mut self) {
        self.record_picker = None;
        self.picker_thumbs.borrow_mut().clear();
    }

    fn start_record_pick(&mut self) {
        let Some(picker) = self.record_picker.take() else { return };
        self.picker_thumbs.borrow_mut().clear();
        let Some(choice) = picker.chosen else { return };
        if let Some(last) = LastPick::of(&choice, picker.windows.as_deref().unwrap_or_default()) {
            save_last_pick(&last);
        }
        if picker.dont_ask {
            self.set_config("recording", "ask_record_target", json!(false));
        }
        self.send(DaemonCommand::RecordPick(choice));
    }

    /// Esc closes, Enter starts; `true` when the picker took the key.
    pub(crate) fn record_picker_key(&mut self, key: &str) -> bool {
        if self.record_picker.is_none() {
            return false;
        }
        match key {
            "escape" => self.close_record_picker(),
            "enter" => self.start_record_pick(),
            "tab" => {
                let next = match self.record_picker.as_ref().map(|p| p.tab) {
                    Some(Tab::Applications) => Tab::Screens,
                    _ => Tab::Applications,
                };
                self.picker_tab(next);
            }
            _ => return false,
        }
        true
    }

    fn picker_tab(&mut self, tab: Tab) {
        if let Some(p) = self.record_picker.as_mut().filter(|p| p.tab != tab) {
            p.tab = tab;
            // The other tab's previews must not stay on screen.
            self.picker_thumbs.borrow_mut().clear();
        }
    }

    pub fn render_record_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if std::mem::take(&mut self.picker_requested) {
            self.open_record_picker(cx);
        }
        let picker = self.record_picker.clone()?;
        // Previews not placed this frame (a card no longer drawn) hide before the next one.
        let thumbs = self.picker_thumbs.clone();
        window.on_next_frame(move |_, _| thumbs.borrow_mut().end_frame());
        let tabs = ui::segment_track()
            .child(ui::segment("pick-apps", "Applications", picker.tab == Tab::Applications).on_click(cx.listener(|this, _, _, cx| {
                this.picker_tab(Tab::Applications);
                cx.notify();
            })))
            .child(ui::segment("pick-screens", "Entire Screen", picker.tab == Tab::Screens).on_click(cx.listener(|this, _, _, cx| {
                this.picker_tab(Tab::Screens);
                cx.notify();
            })));
        let cards: Vec<AnyElement> = match (&picker.windows, picker.tab) {
            (None, _) => vec![note("Looking for windows and screens…")],
            (Some(ws), Tab::Applications) if ws.is_empty() => vec![note("No app window is open. Pick a screen instead.")],
            (Some(ws), Tab::Applications) => ws.iter().map(|w| self.window_card(w, picker.chosen.as_ref(), cx)).collect(),
            (Some(_), Tab::Screens) => picker.monitors.iter().enumerate().map(|(i, m)| screen_card(i, m, &picker.monitors, picker.chosen.as_ref(), cx)).collect(),
        };
        let grid = div()
            .id("pick-grid")
            .max_h(px(440.))
            .overflow_y_scroll()
            .child(div().flex().flex_wrap().gap(px(12.)).children(cards));
        let panel = ui::panel()
            .w(px(720.))
            .max_w_full()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .bg(c(theme::RAISED))
            .border_color(c(theme::BORDER))
            .child(div().font_family(theme::display()).text_size(px(18.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD).child(TITLE))
            .child(tabs)
            .child(grid)
            .child(self.picker_footer(&picker, cx));
        Some(
            div()
                .id("pick-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(ca(0x000000, 0.55))
                .occlude()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(panel)
                .into_any_element(),
        )
    }

    fn window_card(&self, w: &WindowCard, chosen: Option<&Choice>, cx: &mut Context<Self>) -> AnyElement {
        let selected = chosen == Some(&Choice::Window(w.hwnd));
        let (src, thumbs) = (w.hwnd, self.picker_thumbs.clone());
        let image = self.target_icons.get(&w.exe.to_lowercase()).cloned();
        // The live preview: Windows draws it over this box (`dwm_thumbs`); the icon under it
        // shows while it has no picture (a minimised window) or Windows refuses it.
        let preview = div()
            .relative()
            .w_full()
            .h(px(180.))
            .flex()
            .items_center()
            .justify_center()
            .when_some(image.clone(), |d, image| d.child(gpui::img(image).size(px(32.))))
            .child(
                canvas(|_, _, _| (), move |bounds, _, window, _| {
                    let scale = window.scale_factor();
                    let to_px = |b: gpui::Bounds<gpui::Pixels>| {
                        let (o, s) = (b.origin, b.size);
                        [(f32::from(o.x) * scale) as i32, (f32::from(o.y) * scale) as i32, (f32::from(o.x + s.width) * scale) as i32, (f32::from(o.y + s.height) * scale) as i32]
                    };
                    let mask = window.content_mask().bounds;
                    let mut thumbs = thumbs.borrow_mut();
                    if let Ok(own) = crate::root::parent_hwnd(window) {
                        thumbs.set_dest(own);
                    }
                    let inset = gpui::Bounds { origin: bounds.origin + gpui::point(px(6.), px(6.)), size: gpui::size(bounds.size.width - px(12.), bounds.size.height - px(12.)) };
                    thumbs.place(src, to_px(inset), to_px(mask));
                })
                .absolute()
                .inset_0(),
            );
        let caption = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .when_some(image, |d, image| d.child(gpui::img(image).size(px(16.)).flex_none()))
            .child(div().truncate().child(w.label()));
        card(("pick-win", w.hwnd as usize), selected, preview.into_any_element(), caption.into_any_element())
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(p) = this.record_picker.as_mut() {
                    p.chosen = Some(Choice::Window(src));
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn picker_footer(&self, picker: &Picker, cx: &mut Context<Self>) -> AnyElement {
        let height = self.config_value("recording", "height").and_then(Value::as_u64).unwrap_or(0);
        let fps = self.config_value("recording", "fps").and_then(Value::as_u64).unwrap_or(60);
        let ready = picker.chosen.is_some();
        let gear = ui::button("pick-settings", Variant::Ghost).child(icon(Icon::Settings, 16., theme::MUTED)).on_click(cx.listener(|this, _, _, cx| {
            this.close_record_picker();
            this.settings_tab = crate::views::settings::SettingsTab::Recording;
            this.go(crate::root::Route::Settings);
            cx.notify();
        }));
        let quality = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(div().flex().flex_col().child(div().font_weight(FontWeight::SEMIBOLD).child("Recording")).child(div().text_size(px(12.)).text_color(c(theme::MUTED)).child(capture_targets::clip_line(height, fps))))
            .child(gear);
        let dont_ask = div()
            .id("pick-dont-ask")
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .text_color(c(theme::MUTED))
            .child(icon(if picker.dont_ask { Icon::SquareCheck } else { Icon::Square }, 14., if picker.dont_ask { theme::TEXT } else { theme::MUTED }))
            .child(DONT_ASK)
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(p) = this.record_picker.as_mut() {
                    p.dont_ask = !p.dont_ask;
                }
                cx.notify();
            }));
        let cancel = ui::button("pick-cancel", Variant::Secondary).child("Cancel").on_click(cx.listener(|this, _, _, cx| {
            this.close_record_picker();
            cx.notify();
        }));
        let start = ui::button("pick-start", Variant::Primary).when(!ready, |b| b.opacity(0.45)).child(START).on_click(cx.listener(move |this, _, _, cx| {
            if ready {
                this.start_record_pick();
            }
            cx.notify();
        }));
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .child(quality)
            .child(div().flex().items_center().gap(px(12.)).child(dont_ask).child(cancel).child(start))
            .into_any_element()
    }
}

fn note(text: &'static str) -> AnyElement {
    div().w_full().py(px(32.)).flex().justify_center().text_color(c(theme::MUTED)).child(text).into_any_element()
}

/// A card: its picture box, the line under it; two to a row like the reference picker.
fn card(id: (&'static str, usize), selected: bool, picture: AnyElement, caption: AnyElement) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(336.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .cursor_pointer()
        .child(
            div()
                .rounded(px(8.))
                .overflow_hidden()
                .bg(c(theme::BASE))
                .border_2()
                .border_color(c(if selected { theme::FOCUS } else { theme::BORDER_SUBTLE }))
                .hover(|s| s.border_color(c(theme::BORDER_STRONG)))
                .child(picture),
        )
        .child(div().px(px(2.)).font_weight(FontWeight::MEDIUM).text_color(c(if selected { theme::TEXT } else { theme::MUTED })).child(caption))
}

/// A screen card: every monitor drawn in place, this one lit, with its name and size.
fn screen_card(index: usize, m: &MonitorCard, all: &[MonitorCard], chosen: Option<&Choice>, cx: &mut Context<Root>) -> AnyElement {
    let selected = chosen.is_some_and(|ch| matches!(ch, Choice::Monitor(d) if d.eq_ignore_ascii_case(&m.device)));
    let rects = capture_targets::layout_rects(all, 332., 180., 18.);
    let mut layout = div().relative().w_full().h(px(180.));
    for (i, r) in rects.iter().enumerate() {
        let lit = i == index;
        layout = layout.child(
            div()
                .absolute()
                .left(px(r[0]))
                .top(px(r[1]))
                .w(px(r[2] - r[0] - 2.))
                .h(px(r[3] - r[1] - 2.))
                .rounded(px(4.))
                .border_1()
                .border_color(c(if lit { theme::FOCUS } else { theme::BORDER }))
                .bg(c(if lit { theme::TOP } else { theme::OVERLAY })),
        );
    }
    let device = m.device.clone();
    let caption = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(icon(Icon::Fullscreen, 14., theme::MUTED))
        .child(m.name())
        .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(m.detail()));
    card(("pick-screen", index), selected, layout.into_any_element(), caption.into_any_element())
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(p) = this.record_picker.as_mut() {
                p.chosen = Some(Choice::Monitor(device.clone()));
            }
            cx.notify();
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::asks;

    #[test]
    fn record_asks_only_with_the_setting_on_and_two_monitors() {
        assert!(asks(Some(true), 2));
        assert!(!asks(Some(true), 1), "one monitor: Record records");
        assert!(!asks(Some(false), 3), "Don't ask again");
        assert!(!asks(None, 3), "a recorder that does not know the picker");
    }
}
