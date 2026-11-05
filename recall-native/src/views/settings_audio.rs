//! Settings page state, and Settings → General → MIDI device (`midi.input`, "" = off): the
//! input the Audio page's MIDI console listens to. Ports are listed the first time the section
//! draws, off the UI thread. Audio sources live on the Audio page (`audio_page.rs`).
use gpui::{AnyElement, Context, SharedString, deferred, div, prelude::*, px};
use serde_json::{Value, json};

use crate::{
    root::Root,
    services::audio_devices::{self, AudioDevices, DeviceLists},
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    views::pages::{section_title, setting_row},
};

/// Settings page state that lives for the launch: the device lists and which menu is open.
#[derive(Default)]
pub struct SettingsUi {
    pub devices: AudioDevices,
    pub device_menu: Option<DeviceMenu>,
    /// The Shortcuts chip waiting for new keys.
    pub capture: Option<crate::views::settings_keys::ShortcutSlot>,
    /// The Audio page (sources, their lists and menus).
    pub audio: crate::views::audio_page::AudioPage,
    /// Clip → Frame rate's Custom box.
    pub fps: crate::views::settings_fps::FpsInput,
}

/// Which device list is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceMenu {
    Midi,
}

impl DeviceMenu {
    /// The controller config key: (section, key).
    fn key(self) -> (&'static str, &'static str) {
        match self {
            DeviceMenu::Midi => ("midi", "input"),
        }
    }

    fn names(self, lists: &DeviceLists) -> (&[String], Option<&str>) {
        match self {
            DeviceMenu::Midi => (&lists.midi_inputs, None),
        }
    }

    /// Row text for a saved value: "" is off.
    fn label(self, saved: &str, names: Option<&[String]>, default: Option<&str>) -> String {
        if saved.trim().is_empty() {
            return "Off".to_owned();
        }
        audio_devices::device_label(saved, names, default)
    }
}

impl Root {
    /// Start listing devices once per launch (first draw of a section that needs them).
    fn ensure_audio_devices(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.settings_ui.devices, AudioDevices::NotLoaded) {
            return;
        }
        self.settings_ui.devices = AudioDevices::Loading;
        cx.spawn(async move |this, cx| {
            let lists = cx.background_executor().spawn(async { audio_devices::enumerate_blocking() }).await;
            let _ = this.update(cx, |this, cx| {
                this.settings_ui.devices = AudioDevices::Ready(lists);
                cx.notify();
            });
        })
        .detach();
    }

    /// Settings → General → MIDI: the input the Audio page's MIDI console listens to.
    pub fn render_midi_device(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.ensure_audio_devices(cx);
        vec![
            section_title("MIDI"),
            setting_row("MIDI device", "The Audio page shows what this device sends. Off opens no MIDI device.").child(self.device_picker(DeviceMenu::Midi, cx)).into_any_element(),
        ]
    }

    fn device_picker(&self, menu: DeviceMenu, cx: &mut Context<Self>) -> AnyElement {
        let (section, key) = menu.key();
        let saved = self.config_value(section, key).and_then(Value::as_str).unwrap_or("").to_owned();
        let lists = self.settings_ui.devices.lists();
        let (names, default) = lists.map(|l| menu.names(l)).unzip();
        let label = menu.label(&saved, names, default.flatten());
        let open = self.settings_ui.device_menu == Some(menu);
        div()
            .relative()
            .child(
                ui::button(SharedString::from(format!("device-{key}")), Variant::Ghost)
                    .w(px(300.))
                    .justify_between()
                    .border_1()
                    .border_color(c(theme::BORDER))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(icon(Icon::ChevronDown, 14., theme::MUTED))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_ui.device_menu = if this.settings_ui.device_menu == Some(menu) { None } else { Some(menu) };
                        cx.notify();
                    })),
            )
            .when(open, |d| d.child(self.device_menu(menu, &saved, cx)))
            .into_any_element()
    }

    fn device_menu(&self, menu: DeviceMenu, saved: &str, cx: &mut Context<Self>) -> AnyElement {
        let lists = self.settings_ui.devices.lists();
        let (names, default) = lists.map(|l| menu.names(l)).unwrap_or((&[], None));
        let (_, key) = menu.key();
        let mut entries: Vec<(String, String)> = vec![(String::new(), menu.label("", lists.map(|_| names), default))];
        let saved = saved.trim();
        if !saved.is_empty() && lists.is_some() && !names.iter().any(|n| n.trim().eq_ignore_ascii_case(saved)) {
            entries.push((saved.to_owned(), format!("{saved} (not connected)")));
        }
        entries.extend(names.iter().map(|n| (n.clone(), n.clone())));
        let mut panel = div()
            .id(SharedString::from(format!("device-menu-{key}")))
            .occlude()
            .absolute()
            .top(px(38.))
            .right_0()
            .w(px(340.))
            .max_h(px(320.))
            .overflow_y_scroll()
            .p(px(4.))
            .rounded(px(8.))
            .bg(c(theme::FLOATING))
            .border_1()
            .border_color(c(theme::BORDER))
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.settings_ui.device_menu = None;
                cx.notify();
            }));
        if lists.is_none() {
            panel = panel.child(div().h(px(30.)).px(px(10.)).flex().items_center().text_color(c(theme::TERTIARY)).child("Looking for devices…"));
        }
        for (i, (value, label)) in entries.into_iter().enumerate() {
            let selected = value.trim().eq_ignore_ascii_case(saved);
            panel = panel.child(
                div()
                    .id((SharedString::from(format!("device-item-{key}")), i))
                    .h(px(30.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(theme::TOP)))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .when(selected, |d| d.child(icon(Icon::Check, 14., theme::TEXT)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_ui.device_menu = None;
                        let (section, key) = menu.key();
                        this.set_config(section, key, json!(value));
                        cx.notify();
                    })),
            );
        }
        // Deferred like the sort menu: later rows paint after this one and would cover it.
        deferred(panel).with_priority(1).into_any_element()
    }
}
