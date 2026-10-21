//! Settings, split into fixed tabs (user ruling 2026-10-22: "make it general, clip, media
//! and shortcuts as settings menus"): General · Clip · Media · Shortcuts.
use gpui::{AnyElement, Context, SharedString, Window, div, prelude::*, px};
use serde_json::{Value, json};

use crate::{
    root::Root,
    services::daemon::DaemonCommand,
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    views::pages::{section_title, setting_row},
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
    pub fn render_settings(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
            SettingsTab::Clip => self.settings_clip(cx),
            SettingsTab::Media => self.settings_media(cx),
            SettingsTab::Shortcuts => self.settings_shortcuts(),
        });
        self.page_shell_tabs("Settings", "", Some(tabs.into_any_element()), body)
    }

    fn settings_general(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let session = self.snap.session.clone().unwrap_or_default();
        let overlay_on = self.config_value("overlay", "enabled").and_then(Value::as_bool).unwrap_or(session.overlay.enabled);
        let corner = self.config_value("overlay", "corner").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| session.overlay.corner.clone());
        let autostart = self.config_value("startup", "autostart").and_then(Value::as_bool).unwrap_or(session.autostart);
        let mut corners = ui::segment_track().w(px(360.));
        for (value, label) in [("top-left", "Top left"), ("top-right", "Top right"), ("bottom-left", "Bottom left"), ("bottom-right", "Bottom right")] {
            corners = corners.child(ui::segment(SharedString::from(format!("corner-{value}")), label, corner == value).on_click(cx.listener(move |this, _, _, cx| {
                this.set_config("overlay", "corner", json!(value));
                cx.notify();
            })));
        }
        vec![
            section_title("Startup"),
            setting_row("Start with Windows", "Recall starts in the tray when you sign in and records your games.")
                .child(ui::switch("autostart", autostart).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_config("startup", "autostart", json!(!autostart));
                    cx.notify();
                })))
                .into_any_element(),
            section_title("Overlay"),
            setting_row("Show a notice on save", "A small corner card for 3 seconds. Hidden from your own captures.")
                .child(ui::switch("overlay-on", overlay_on).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_config("overlay", "enabled", json!(!overlay_on));
                    cx.notify();
                })))
                .into_any_element(),
            setting_row("Corner", "").child(corners).into_any_element(),
            section_title("Recorder"),
            setting_row("Quit Recall", "Stops recording and closes the tray icon.")
                .child(ui::button("quit-recorder", Variant::Ghost).border_1().border_color(c(theme::BORDER)).child(icon(Icon::Power, 14., theme::MUTED)).child("Stop recorder").on_click(cx.listener(|this, _, _, cx| {
                    this.send(DaemonCommand::Quit);
                    cx.notify();
                })))
                .into_any_element(),
        ]
    }

    fn settings_clip(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
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
    }

    fn settings_media(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let out_dir = self
            .config_value("recording", "output_dir")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| std::env::var("RECALL_OUT_DIR").ok())
            .unwrap_or_else(|| r"V:\ALL RECORDINGS".into());
        let folder = out_dir.clone();
        let mut items = vec![
            section_title("Clips"),
            setting_row("Clip folder", &out_dir)
                .child(ui::button("open-folder", Variant::Secondary).child(icon(Icon::Folder, 14., theme::TEXT)).child("Open").on_click(cx.listener(move |_, _, _, _| {
                    let _ = std::process::Command::new("explorer.exe").arg(&folder).spawn();
                })))
                .into_any_element(),
        ];
        items.extend(self.render_storage_section(cx));
        items
    }

    fn settings_shortcuts(&self) -> Vec<AnyElement> {
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
        let save = if session.save_hotkey.is_empty() { "Alt+N".to_owned() } else { session.save_hotkey.clone() };
        let mic = if session.mic_hotkey.is_empty() { "Alt+M".to_owned() } else { session.mic_hotkey.clone() };
        vec![
            div()
                .text_size(px(12.))
                .text_color(c(theme::TERTIARY))
                .child("Recorder shortcuts work in every game, even when Recall is closed.")
                .into_any_element(),
            group("Recorder", vec![("Save replay", save), ("Mute microphone in clips", mic)]),
            group(
                "Library",
                vec![
                    ("Search", "Ctrl+K".into()),
                    ("Select several clips", "Ctrl+Click / Shift+Click".into()),
                    ("Select all", "Ctrl+A".into()),
                    ("Delete selected", "Del".into()),
                    ("Clear selection", "Esc".into()),
                    ("Rename", "Double-click".into()),
                ],
            ),
            group(
                "Editor",
                vec![
                    ("Play / pause", "Space".into()),
                    ("Set in / set out", "I / O".into()),
                    ("Split", "C".into()),
                    ("Delete segment", "Del".into()),
                    ("Undo / redo", "Ctrl+Z / Ctrl+Y".into()),
                    ("Step a frame", "← / →".into()),
                    ("Jump 1 second", "Shift+← / Shift+→".into()),
                    ("Back / forward 5 s", "J / L".into()),
                    ("Back to library", "Esc".into()),
                ],
            ),
        ]
    }
}
