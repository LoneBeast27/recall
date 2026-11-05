//! Settings, split into fixed tabs (user ruling 2026-10-22: "make it general, clip, media
//! and shortcuts as settings menus"): General · Clip · Media · Shortcuts.
use gpui::{AnyElement, Context, SharedString, Window, div, prelude::*, px};
use serde_json::{Value, json};

use crate::{
    root::Root,
    services::daemon::DaemonCommand,
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    views::{
        pages::{section_title, setting_row},
        settings_keys::ShortcutSlot,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    General,
    Clip,
    Media,
    Shortcuts,
}

impl SettingsTab {
    const ALL: [SettingsTab; 4] = [SettingsTab::General, SettingsTab::Clip, SettingsTab::Media, SettingsTab::Shortcuts];

    fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "General",
            SettingsTab::Clip => "Clip",
            SettingsTab::Media => "Media",
            SettingsTab::Shortcuts => "Shortcuts",
        }
    }
}

impl Root {
    pub fn render_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let current = self.settings_tab;
        let mut tabs = ui::segment_track().w(px(440.));
        for tab in SettingsTab::ALL {
            tabs = tabs.child(ui::segment(SharedString::from(format!("settings-{}", tab.label())), tab.label(), tab == current).on_click(cx.listener(move |this, _, _, cx| {
                this.settings_tab = tab;
                if tab == SettingsTab::Media {
                    this.cache.refresh();
                }
                cx.notify();
            })));
        }
        let mut body: Vec<AnyElement> = Vec::new();
        body.extend(self.offline_banner(cx));
        body.extend(match current {
            SettingsTab::General => self.settings_general(cx),
            SettingsTab::Clip => self.settings_clip(window, cx),
            SettingsTab::Media => self.settings_media(cx),
            SettingsTab::Shortcuts => self.settings_shortcuts(cx),
        });
        self.page_shell_tabs("Settings", "", Some(tabs.into_any_element()), body)
    }

    fn settings_general(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let session = self.snap.session.clone().unwrap_or_default();
        let overlay_on = self.config_value("overlay", "enabled").and_then(Value::as_bool).unwrap_or(session.overlay.enabled);
        let corner = self.config_value("overlay", "corner").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| session.overlay.corner.clone());
        let autostart = self.config_value("startup", "autostart").and_then(Value::as_bool).unwrap_or(session.autostart);
        let stats_on = self.stats_on();
        let leader = if session.leader_label.is_empty() { "Alt `".to_owned() } else { session.leader_label.clone() };
        let stats_detail = format!("FPS and 1% low, GPU and CPU in a small island over your game. {leader} then 5 also turns it on or off.");
        let fps_line = fps_source_status(&session.fps_source).filter(|_| stats_on).map(|(text, color)| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(ui::dot(color, 6.))
                .child(div().text_size(px(12.)).text_color(c(color)).child(text))
                .into_any_element()
        });
        let mut corners = ui::segment_track().w(px(360.));
        for (value, label) in [("top-left", "Top left"), ("top-right", "Top right"), ("bottom-left", "Bottom left"), ("bottom-right", "Bottom right")] {
            corners = corners.child(ui::segment(SharedString::from(format!("corner-{value}")), label, corner == value).on_click(cx.listener(move |this, _, _, cx| {
                this.set_config("overlay", "corner", json!(value));
                cx.notify();
            })));
        }
        let mut items = vec![
            section_title("Startup"),
            setting_row("Start with Windows", "Recall starts in the tray when you sign in and records your games.")
                .child(ui::switch("autostart", autostart).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_config("startup", "autostart", json!(!autostart));
                    cx.notify();
                })))
                .into_any_element(),
            section_title("Overlay"),
            setting_row("Show notices", "Saves, bookmarks and recording changes appear for 3 seconds. Hidden from your own captures.")
                .child(ui::switch("overlay-on", overlay_on).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_config("overlay", "enabled", json!(!overlay_on));
                    cx.notify();
                })))
                .into_any_element(),
            setting_row("Performance stats", &stats_detail)
                .child(ui::switch("overlay-stats", stats_on).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_config("overlay", "stats", json!(!stats_on));
                    cx.notify();
                })))
                .into_any_element(),
        ];
        items.extend(fps_line);
        let corner_row = setting_row("Corner", "").child(corners).into_any_element();
        items.push(corner_row);
        items.extend(self.render_midi_device(cx));
        items.extend([
            section_title("Recorder"),
            setting_row("Quit Recall", "Stops recording and closes the tray icon.")
                .child(ui::button("quit-recorder", Variant::Ghost).border_1().border_color(c(theme::BORDER)).child(icon(Icon::Power, 14., theme::MUTED)).child("Stop recorder").on_click(cx.listener(|this, _, _, cx| {
                    this.send(DaemonCommand::Quit);
                    cx.notify();
                })))
                .into_any_element(),
        ]);
        items
    }

    /// The performance-stats island is on (config first, then the live status).
    pub(crate) fn stats_on(&self) -> bool {
        let session = self.snap.session.as_ref().is_some_and(|s| s.overlay_stats);
        self.config_value("overlay", "stats").and_then(Value::as_bool).unwrap_or(session)
    }

    fn settings_clip(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let session = self.snap.session.clone().unwrap_or_default();
        let auto_record = self.config_value("recording", "auto_record").and_then(Value::as_bool).unwrap_or(session.auto_record);
        let desktop = self.config_bool("recording", "record_desktop_when_idle");
        let buffer = self.config_value("recording", "buffer_secs").and_then(Value::as_u64).unwrap_or(session.save_seconds.max(1) as u64);
        let mut lengths = ui::segment_track().w(px(300.));
        for (secs, label) in [(30u64, "30 s"), (60, "1 min"), (120, "2 min"), (300, "5 min")] {
            lengths = lengths.child(ui::segment(("len", secs as usize), label, buffer == secs).on_click(cx.listener(move |this, _, _, cx| {
                this.set_config("recording", "buffer_secs", json!(secs));
                cx.notify();
            })));
        }
        vec![
            section_title("Recording"),
            setting_row("Record games automatically", "Starts when a fullscreen game is in front; stops when it closes.")
                .child(ui::switch("auto-record", auto_record).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_config("recording", "auto_record", json!(!auto_record));
                    cx.notify();
                })))
                .into_any_element(),
            setting_row("Record the desktop between games", "Keeps a replay of your main screen when no game runs.")
                .child(ui::switch("desktop-idle", desktop).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_config("recording", "record_desktop_when_idle", json!(!desktop));
                    cx.notify();
                })))
                .into_any_element(),
            setting_row("Replay length", "How much a save keeps. Longer uses more memory.").child(lengths).into_any_element(),
        ]
        .into_iter()
        .chain(self.render_video_section(window, cx))
        .collect()
    }

    fn settings_media(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut items = self.render_folder_section(cx);
        items.extend(self.render_storage_limit(cx));
        items.push(section_title("SSD cache"));
        items.extend(self.render_storage_section(cx));
        items
    }

    fn settings_shortcuts(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let session = self.snap.session.clone().unwrap_or_default();
        let group = |title: &str, rows: Vec<(&str, String)>| -> AnyElement {
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().pb(px(6.)).child(ui::caption(title.to_owned())))
                .children(rows.into_iter().map(|(label, keys)| {
                    div()
                        .h(px(34.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(c(theme::BORDER_SUBTLE))
                        .child(div().child(label.to_owned()))
                        .child(div().flex().gap(px(4.)).children(keys.split(" / ").map(|k| ui::kbd(k.replace('+', " ")))))
                }))
                .into_any_element()
        };
        let direct: Vec<AnyElement> = ShortcutSlot::DIRECT.into_iter().map(|slot| self.shortcut_row(slot, cx)).collect();
        let advanced = self.config_value("hotkeys", "advanced_enabled").and_then(Value::as_bool).unwrap_or(session.advanced_enabled);
        let leader_accel = self.shortcut_value(ShortcutSlot::Leader);
        let backquote = leader_accel.to_ascii_lowercase().ends_with("backquote");
        let leader = if backquote && !session.leader_label.is_empty() { session.leader_label.clone() } else { crate::views::settings_keys::chip_text(&leader_accel) };
        let where_key = if backquote { " (the key left of 1)" } else { "" };
        let chord_detail = format!("Press {leader}{where_key}, then a number. The game never sees the number.");
        let chord_secs = self.config_value("hotkeys", "save_seconds").and_then(Value::as_u64).unwrap_or(if session.chord_save_seconds > 0 { session.chord_save_seconds as u64 } else { 30 });
        let save_label = format!("Save the last {}", short_length(chord_secs));
        vec![
            div()
                .text_size(px(12.))
                .text_color(c(theme::TERTIARY))
                .child("These work in every game, even when Recall is closed.")
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().pb(px(6.)).child(ui::caption("While you play — click a key to change it")))
                .children(direct)
                .into_any_element(),
            section_title("Two-step shortcuts"),
            setting_row("Use two-step shortcuts", &chord_detail)
                .child(ui::switch("advanced-shortcuts", advanced).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_config("hotkeys", "advanced_enabled", json!(!advanced));
                    cx.notify();
                })))
                .into_any_element(),
            div()
                .when(!advanced, |d| d.opacity(0.4))
                .child(self.shortcut_row(ShortcutSlot::Leader, cx))
                .child(group(
                    "Then press",
                    vec![
                        ("Bookmark this moment", "1".to_owned()),
                        (save_label.as_str(), "2".to_owned()),
                        ("Screenshot", "3".to_owned()),
                        ("Start or stop a recording", "4".to_owned()),
                        ("Performance stats", "5".to_owned()),
                    ],
                ))
                .into_any_element(),
        ]
    }
}

/// "30 s", "2 min", "1:30".
fn short_length(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{}:{:02}", s / 60, s % 60),
    }
}

/// The one-line FPS source under the Performance stats switch (status
/// `fps_source`); none while stats are off or on controllers without it.
pub(crate) fn fps_source_status(fps_source: &str) -> Option<(&'static str, u32)> {
    Some(match fps_source {
        "etw" => ("FPS from Windows frame timing", theme::ACTIVE),
        "no_permission" => ("FPS needs Performance Log Users — run the installer", theme::WARNING),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_source_status_lines() {
        assert_eq!(fps_source_status("etw"), Some(("FPS from Windows frame timing", theme::ACTIVE)));
        assert_eq!(
            fps_source_status("no_permission"),
            Some(("FPS needs Performance Log Users — run the installer", theme::WARNING))
        );
        assert_eq!(fps_source_status("off"), None);
        assert_eq!(fps_source_status(""), None, "older controllers say nothing");
        assert_eq!(fps_source_status("running"), None, "old RTSS states are not shown");
    }
}
