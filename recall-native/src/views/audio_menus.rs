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
    /// A strip's device: the source, the name, the endpoint id.
    Device(String, String, String),
    App(String, String, String),
    Add(Kind),
    /// A new input: a microphone (true) or a line-in or capture card (false).
    AddInput(bool),
    /// An input's "Muted by the mic hotkey" (the source, the new value).
    MicMute(String, bool),
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

/// A checked row that does nothing on click (muted text): a state the strip cannot change.
fn inert_row(leading: Option<AnyElement>, label: &str) -> AnyElement {
    div()
        .min_h(px(30.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(8.))
        .text_color(c(theme::TERTIARY))
        .children(leading)
        .child(div().flex_1().min_w_0().truncate().child(label.to_owned()))
        .child(icon(Icon::Check, 14., theme::TERTIARY))
        .into_any_element()
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
            Pick::Device(id, device, device_id) => self.set_source_device(&id, device, device_id),
            Pick::App(id, exe, label) if id.is_empty() => self.add_source(Kind::App, &exe, &label),
            Pick::App(id, exe, label) => self.set_source_app(&id, exe, label),
            Pick::Add(kind) => self.add_source(kind, "", ""),
            Pick::AddInput(mic) => self.add_input(mic),
            Pick::MicMute(id, on) => self.set_source_mic_mute(&id, on),
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
            // (name, endpoint id, label): an input saves the endpoint id too.
            let mut entries = vec![(String::new(), String::new(), audio_devices::device_label("", lists.map(|_| names.as_slice()), default.as_deref()))];
            let matched = (!saved.is_empty()).then(|| crate::services::meters::match_endpoint(&names, &saved)).flatten();
            // A twin name is listed already (marked "1 of 2"): no extra "not connected" row.
            if !saved.is_empty() && lists.is_some() && matched.is_none() && !names.iter().any(|n| n.trim() == saved) {
                entries.push((saved.clone(), source.device_id.clone(), format!("{saved} (not connected)")));
            }
            let first_name = entries.len();
            let ids = lists.filter(|_| source.kind.uses_inputs()).map(|l| l.input_ids.clone()).unwrap_or_default();
            for (i, (name, label)) in names.iter().cloned().zip(audio_devices::picker_labels(&names)).enumerate() {
                let id = if source.kind.uses_inputs() { ids.get(i).cloned().unwrap_or_default() } else { String::new() };
                entries.push((name, id, label));
            }
            for (i, (value, value_id, label)) in entries.into_iter().enumerate() {
                // The endpoint the recorder opens for the saved name (its real name may differ).
                let selected = match matched {
                    Some(m) => i == first_name + m,
                    None => value.trim() == saved,
                };
                rows.push(self.menu_row((&key, i), None, label, selected, Pick::Device(source.id.clone(), value, value_id), cx));
            }
            if source.kind == Kind::Input {
                // A capture card or console line-in keeps its sound while the mic is muted; on
                // the Windows default there is nothing to turn off (R5-deep-audio-3).
                if !saved.is_empty() {
                    rows.push(note("Turn this off for a line-in or capture card:"));
                }
                // The form factor only hints; Alt+M follows the strip's flag (none = muted).
                if crate::services::audio_sources::windows_says_line_level(source, lists) {
                    rows.push(note("Windows reports this as a line-level input, not a microphone."));
                }
                // On the Windows default a line-in records the default input (normally the mic).
                if crate::services::audio_sources::line_in_on_default(source) {
                    rows.push(note(crate::services::audio_sources::LINE_IN_ON_DEFAULT));
                }
                // A line-in with no saved id, or on another endpoint, follows Alt+M (R6-deep-audio-1).
                if let Some(text) = crate::services::audio_mute::other_note(source, lists) {
                    rows.push(note(text));
                }
                let glyph = Some(icon(Icon::MicOff, 14., theme::MUTED).into_any_element());
                // The strip's own flag; with no device the row is checked and inert: the Windows
                // default is always muted (R5-deep-audio-3).
                match crate::services::audio_mute::mic_row(source) {
                    (checked, Some(on)) => rows.push(self.menu_row((&key, 950), glyph, "Muted by the mic hotkey".into(), checked, Pick::MicMute(source.id.clone(), on), cx)),
                    (_, None) => rows.push(inert_row(glyph, "Muted by the mic hotkey")),
                }
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
            let choices = crate::services::audio_sources::addable(list).into_iter().flat_map(|kind| match kind {
                Kind::Input => vec![(Icon::Mic, "Microphone", Pick::AddInput(true)), (Icon::Mic, "Line-in or capture card", Pick::AddInput(false))],
                Kind::Output => vec![(Icon::Volume, "Desktop or output device", Pick::Add(kind))],
                Kind::App => vec![(Icon::AudioLines, "Application audio", Pick::OpenApps)],
                Kind::Game => vec![(Icon::Gamepad, "Game audio", Pick::Add(kind))],
            });
            for (i, (glyph, label, pick)) in choices.enumerate() {
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
