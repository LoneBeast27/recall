//! In-memory storage and device fixtures for the standalone RecallTesting build.
use gpui::{AnyElement, Context, FontWeight, div, prelude::*, px};
use serde_json::{Value, json};

use crate::{
    root::Root,
    services::{audio_apps::{AppList, PlayingApp}, audio_devices::DeviceLists},
    theme::{self, c},
    ui::{self, Variant},
    views::pages::{card, setting_row},
};

pub(super) fn audio_lists() -> (DeviceLists, AppList) {
    (
        DeviceLists {
            inputs: vec!["Demo microphone".into(), "Demo capture card".into()],
            input_ids: vec!["{demo-mic}".into(), "{demo-line}".into()],
            input_forms: vec![None, Some(2)],
            outputs: vec!["Demo speakers".into(), "Demo headset".into()],
            default_input: Some("Demo microphone".into()),
            default_output: Some("Demo speakers".into()),
            communications_output: Some("Demo headset".into()),
            line_inputs: vec!["Demo capture card".into()],
            ..DeviceLists::default()
        },
        AppList {
            apps: [("Discord.exe", "Discord"), ("Spotify.exe", "Spotify"), ("obs64.exe", "OBS Studio")]
                .into_iter().map(|(exe, label)| PlayingApp { exe: exe.into(), label: label.into(), path: String::new() }).collect(),
            ..AppList::default()
        },
    )
}

impl Root {
    pub(super) fn render_testing_storage(&self, cx: &mut Context<Self>) -> AnyElement {
        let value = |key, default| self.config_value("storage", key).and_then(Value::as_u64).unwrap_or(default);
        let choices = |items: &[(u64, &str)]| items.iter().map(|(n, label)| (*n, (*label).to_owned())).collect::<Vec<_>>();
        let drive = self.segments("test-drive", 230., &choices(&[(0, "Demo SSD"), (1, "Demo archive")]),
            value("demo_drive", 0), "storage", "demo_drive", |n| json!(n), cx);
        let cap = self.segments("test-cap", 230., &choices(&[(0, "Unlimited"), (100, "100 GB"), (250, "250 GB")]),
            value("cap_gb", 0), "storage", "cap_gb", |n| json!(n), cx);
        let cache = self.segments("test-cache", 230., &choices(&[(2, "2 GiB"), (10, "10 GiB"), (20, "20 GiB")]),
            value("cache_gib", 2), "storage", "cache_gib", |n| json!(n), cx);
        let used = format!("{} MiB of simulated preview data.", value("cached_mib", 512));
        self.page_shell("Storage", "Choose storage for this setup rehearsal.", vec![
            card().child(div().font_weight(FontWeight::MEDIUM).child("Demo drives and cache"))
                .child(div().text_color(c(theme::MUTED)).child("Choices apply to this session’s demo data."))
                .into_any_element(),
            setting_row("Clip location", "Choose between two simulated destinations.").child(drive).into_any_element(),
            setting_row("Clip storage limit", "Rehearse the storage cap on a demo drive.").child(cap).into_any_element(),
            setting_row("SSD cache size", "Choose a simulated preview cache budget.").child(cache).into_any_element(),
            setting_row("Demo cache", &used)
                .child(ui::button("test-cache-clear", Variant::Secondary).child("Clear demo cache")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_config("storage", "cached_mib", json!(0));
                        cx.notify();
                    }))).into_any_element(),
        ])
    }

    pub(crate) fn render_testing_library(&self, cx: &mut Context<Self>) -> AnyElement {
        ui::panel().size_full().child(self.page_shell("Demo library", "Setup rehearsal complete.", vec![
            card().gap(px(12.))
                .child(div().font_weight(FontWeight::MEDIUM).child("Your setup choices are active for this demo"))
                .child(div().text_color(c(theme::MUTED)).child("This library starts empty. Reopen setup to try another configuration."))
                .child(ui::button("test-reopen-setup", Variant::Secondary).child("Reopen setup")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.begin_setup(false);
                        cx.notify();
                    })))
                .into_any_element(),
        ])).into_any_element()
    }
}
