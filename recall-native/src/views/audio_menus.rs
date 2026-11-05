//! Audio page menus: a strip's device or app picker, and "+ Add source" (Microphone or line-in,
//! Desktop or output device, Application audio, Game audio). Device lists are the ones read
//! when the page opened; "Windows default" comes first and a saved device that is gone shows
//! as "<name> (not connected)".
use std::sync::Arc;

use gpui::{AnyElement, Context, ImageSource, PathPromptOptions, RenderImage, SharedString, Stateful, deferred, div, img, prelude::*, px};

use crate::{
    root::Root,
    services::{
        audio_apps,
        audio_devices,
        audio_sources::{Kind, Source},
    },
    theme::{self, c},
    ui::{Icon, icon},
    views::audio_page::StripMenu,
};

/// What a menu row does.
#[derive(Clone)]
enum Pick {
    Device(String, String),
    App(String, String, String),
    Add(Kind),
    OpenApps,
    Browse(Option<String>),
}

fn panel(id: SharedString, width: f32) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .occlude()
        .absolute()
        .top(px(36.))
        .w(px(width))
        .max_h(px(320.))
        .overflow_y_scroll()
        .p(px(4.))
        .rounded(px(8.))
        .bg(c(theme::FLOATING))
        .border_1()
        .border_color(c(theme::BORDER))
        .flex()
        .flex_col()
}

fn note(text: &str) -> AnyElement {
    div().px(px(10.)).py(px(6.)).text_size(px(11.)).line_height(px(15.)).text_color(c(theme::TERTIARY)).child(text.to_owned()).into_any_element()
}

impl Root {
    fn menu_row(&self, (key, i): (&str, usize), leading: Option<AnyElement>, label: String, selected: bool, pick: Pick, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id((SharedString::from(format!("menu-{key}")), i))
            .min_h(px(30.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(5.))
            .cursor_pointer()
            .hover(|s| s.bg(c(theme::TOP)))
            .children(leading)
            .child(div().flex_1().min_w_0().truncate().child(label))
            .when(selected, |d| d.child(icon(Icon::Check, 14., theme::TEXT)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.apply_pick(pick.clone(), cx);
                cx.notify();
            }))
            .into_any_element()
    }

    fn apply_pick(&mut self, pick: Pick, cx: &mut Context<Self>) {
        let page = &mut self.settings_ui.audio;
        page.menu = None;
        match pick {
            Pick::Device(id, device) => self.set_source_device(&id, device),
            Pick::App(id, exe, label) if id.is_empty() => self.add_source(Kind::App, &exe, &label),
            Pick::App(id, exe, label) => self.set_source_app(&id, exe, label),
            Pick::Add(kind) => self.add_source(kind, "", ""),
            Pick::OpenApps => self.settings_ui.audio.menu = Some(StripMenu::AddApp),
            Pick::Browse(id) => self.browse_for_app(id, cx),
        }
    }

    /// Any exe on disk, for an app that is not playing right now.
    fn browse_for_app(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(SharedString::from("Record this app")) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))) else { return };
            let app = cx.background_executor().spawn(async move { audio_apps::app_from_path(&path) }).await;
            let _ = this.update(cx, |this, cx| {
                match &id {
                    Some(id) => this.set_source_app(id, app.exe, app.label),
                    None => this.add_source(Kind::App, &app.exe, &app.label),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn app_icon(&self, exe: &str) -> AnyElement {
        match self.settings_ui.audio.icon(exe) {
            Some(image) => img(ImageSource::Render(image as Arc<RenderImage>)).size(px(16.)).flex_none().into_any_element(),
            None => icon(Icon::AudioLines, 16., theme::MUTED).into_any_element(),
        }
    }

    /// Rows for the playing apps; `id` = the source they go to, `None` = a new source.
    fn app_rows(&self, key: &str, id: Option<&str>, current: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let page = &self.settings_ui.audio;
        let mut rows = Vec::new();
        match page.apps.as_ref() {
            None => rows.push(note("Looking for apps playing sound…")),
            Some(list) if list.apps.is_empty() => rows.push(note("No app is playing sound right now. Start it, or browse for its exe.")),
            Some(list) => {
                for (i, app) in list.apps.clone().into_iter().enumerate() {
                    let pick = match id {
                        Some(id) => Pick::App(id.to_owned(), app.exe.clone(), app.label.clone()),
                        None => Pick::App(String::new(), app.exe.clone(), app.label.clone()),
                    };
                    let selected = app.exe.eq_ignore_ascii_case(current);
                    rows.push(self.menu_row((key, i), Some(self.app_icon(&app.exe)), app.label.clone(), selected, pick, cx));
                }
            }
        }
        rows.push(self.menu_row((key, 900), Some(icon(Icon::Folder, 14., theme::MUTED).into_any_element()), "Browse for an app…".into(), false, Pick::Browse(id.map(str::to_owned)), cx));
        rows
    }

    /// The picker under a strip's device button.
    pub(crate) fn device_menu_for(&self, source: &Source, cx: &mut Context<Self>) -> AnyElement {
        let key = format!("dev-{}", source.id);
        let mut rows: Vec<AnyElement> = Vec::new();
        if source.kind == Kind::App {
            rows.extend(self.app_rows(&key, Some(&source.id), &source.exe, cx));
        } else {
            let lists = self.settings_ui.audio.devices.as_ref();
            let (names, default) = match lists {
                Some(l) if source.kind.uses_inputs() => (l.inputs.clone(), l.default_input.clone()),
                Some(l) => (l.outputs.clone(), l.default_output.clone()),
                None => (Vec::new(), None),
            };
            if source.kind == Kind::Game {
                rows.push(note("Games record their own sound. This output is used when Recall records the desktop."));
            }
            if lists.is_none() {
                rows.push(note("Looking for devices…"));
            }
            let saved = source.device.trim().to_owned();
            let mut entries = vec![(String::new(), audio_devices::device_label("", lists.map(|_| names.as_slice()), default.as_deref()))];
            if !saved.is_empty() && lists.is_some() && !names.iter().any(|n| n.trim().eq_ignore_ascii_case(&saved)) {
                entries.push((saved.clone(), format!("{saved} (not connected)")));
            }
            entries.extend(names.iter().map(|n| (n.clone(), n.clone())));
            for (i, (value, label)) in entries.into_iter().enumerate() {
                let selected = value.trim().eq_ignore_ascii_case(&saved);
                rows.push(self.menu_row((&key, i), None, label, selected, Pick::Device(source.id.clone(), value), cx));
            }
        }
        let panel = panel(SharedString::from(format!("strip-menu-{}", source.id)), 280.).left_0().children(rows).on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.settings_ui.audio.menu = None;
            cx.notify();
        }));
        deferred(panel).with_priority(1).into_any_element()
    }

    /// "+ Add source": the kinds, then the playing apps for Application audio.
    /// `right`: opened from the card header, so the menu hangs left from the button's right edge.
    pub(crate) fn add_menu(&self, list: &[Source], right: bool, cx: &mut Context<Self>) -> AnyElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        if self.settings_ui.audio.menu == Some(StripMenu::AddApp) {
            rows.push(note("Record one app's sound on its own track:"));
            rows.extend(self.app_rows("add-app", None, "", cx));
        } else {
            for (i, kind) in crate::services::audio_sources::addable(list).into_iter().enumerate() {
                let (glyph, label, pick) = match kind {
                    Kind::Input => (Icon::Mic, "Microphone or line-in", Pick::Add(kind)),
                    Kind::Output => (Icon::Volume, "Desktop or output device", Pick::Add(kind)),
                    Kind::App => (Icon::AudioLines, "Application audio", Pick::OpenApps),
                    Kind::Game => (Icon::Gamepad, "Game audio", Pick::Add(kind)),
                };
                rows.push(self.menu_row(("add", i), Some(icon(glyph, 14., theme::MUTED).into_any_element()), label.into(), false, pick, cx));
            }
        }
        let panel = panel("add-menu".into(), 260.).when(right, |p| p.right_0()).when(!right, |p| p.left_0()).children(rows).on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.settings_ui.audio.menu = None;
            cx.notify();
        }));
        deferred(panel).with_priority(1).into_any_element()
    }
}
