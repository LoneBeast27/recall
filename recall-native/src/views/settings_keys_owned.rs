//! Settings → Shortcuts: a shortcut stored on a combo Windows owns (Alt+F4, Alt+Tab…). The
//! recorder keeps it in the file (old settings load unchanged) but never registers it, and sends
//! it as `windows_owned`. Here it gets one line with its fix: Pick another (opens the chip) or
//! Turn off (optional shortcuts only); either change then offers Undo once.
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use serde_json::json;

use crate::{
    root::Root,
    services::daemon::OwnedShortcut,
    theme::{self, c},
    ui,
    views::settings_keys::ShortcutSlot,
};

/// The line's text for one entry.
pub fn owned_line(entry: &OwnedShortcut) -> String {
    format!("{} uses {}, which Windows keeps ({}), so Recall does not use it", capitalised(&entry.label), entry.accel, entry.why)
}

fn capitalised(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|f| f.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

impl Root {
    /// The `windows_owned` entry behind `slot`'s chip, if its combo is one Windows owns.
    pub(crate) fn owned_slot(&self, slot: ShortcutSlot) -> Option<OwnedShortcut> {
        let save = self.shortcut_value(ShortcutSlot::Save);
        self.snap
            .windows_owned
            .iter()
            .find(|e| ShortcutSlot::from_config_key(&e.key, &e.accel, &save) == Some(slot))
            .cloned()
    }

    /// One line per Windows-owned shortcut, then the Undo line after a fix.
    pub(crate) fn windows_owned_lines(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let save = self.shortcut_value(ShortcutSlot::Save);
        let mut lines: Vec<AnyElement> = self
            .snap
            .windows_owned
            .iter()
            .map(|entry| {
                let slot = ShortcutSlot::from_config_key(&entry.key, &entry.accel, &save);
                let mut row = div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(c(theme::WARNING)).child(owned_line(entry)));
                if let Some(slot) = slot {
                    let id = slot_id(slot);
                    row = row.child(ui::button(SharedString::from(format!("owned-pick-{id}")), ui::Variant::Secondary).child("Pick another").on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.settings_ui.capture = Some(slot);
                            this.settings_ui.capture_note = None;
                            window.focus(&this.focus);
                            cx.notify();
                        }),
                    ));
                    if slot.optional() {
                        let old = entry.accel.clone();
                        row = row.child(ui::button(SharedString::from(format!("owned-off-{id}")), ui::Variant::Ghost).child("Turn off").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.settings_ui.shortcut_undo = Some((slot, old.clone()));
                                let (section, key) = slot_key(slot);
                                this.set_config(section, key, json!(""));
                                cx.notify();
                            }),
                        ));
                    }
                }
                row.into_any_element()
            })
            .collect();
        if let Some((slot, old)) = self.settings_ui.shortcut_undo.clone() {
            let text = format!("{} changed from {}", slot.label(), old);
            lines.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(text))
                    .child(ui::button("owned-undo", ui::Variant::Ghost).child("Undo").on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_ui.shortcut_undo = None;
                        let (section, key) = slot_key(slot);
                        this.set_config(section, key, json!(old.clone()));
                        cx.notify();
                    })))
                    .into_any_element(),
            );
        }
        lines
    }
}

fn slot_key(slot: ShortcutSlot) -> (&'static str, &'static str) {
    slot.config_key()
}

fn slot_id(slot: ShortcutSlot) -> &'static str {
    slot.config_key().1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_names_the_shortcut_the_combo_and_what_windows_does() {
        let entry = OwnedShortcut {
            key: "recording.screenshot_hotkey".into(),
            label: "screenshot".into(),
            accel: "Alt+F4".into(),
            why: "it closes the window in front".into(),
        };
        assert_eq!(owned_line(&entry), "Screenshot uses Alt+F4, which Windows keeps (it closes the window in front), so Recall does not use it");
    }
}
