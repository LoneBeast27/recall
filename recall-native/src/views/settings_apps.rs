//! Settings → Clip → Audio → App audio: apps recorded as tracks of their own
//! (`recording.app_audio`, a list of `{exe, label}` in track order). "Add app…" lists the apps
//! that have sound right now, fetched off the UI thread each time it opens; "Browse…" takes any
//! exe. A change restarts the recorder like the other audio sources.
use gpui::{AnyElement, Context, PathPromptOptions, SharedString, deferred, div, prelude::*, px};
use serde_json::{Value, json};

use crate::{
    root::Root,
    services::audio_apps::{self, PlayingApp, PlayingApps},
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    views::pages::setting_row,
};

/// Most apps the list holds (the controller's cap).
pub const APPS_MAX: usize = 6;

/// The saved list from the controller's config value.
pub fn saved_apps(value: Option<&Value>) -> Vec<PlayingApp> {
    value
        .and_then(Value::as_array)
        .map(|apps| {
            apps.iter()
                .filter_map(|app| {
                    let exe = app.get("exe")?.as_str()?.to_owned();
                    let label = app.get("label").and_then(Value::as_str).unwrap_or("").to_owned();
                    Some(PlayingApp { label: if label.trim().is_empty() { audio_apps::exe_stem(&exe) } else { label }, exe })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `apps` with `app` added at the end; `None` when it is already there or the list is full.
pub fn with_app(apps: &[PlayingApp], app: PlayingApp) -> Option<Vec<PlayingApp>> {
    let exe = app.exe.trim();
    if exe.is_empty() || apps.len() >= APPS_MAX || apps.iter().any(|a| a.exe.trim().eq_ignore_ascii_case(exe)) {
        return None;
    }
    let mut out = apps.to_vec();
    out.push(PlayingApp { exe: exe.to_owned(), label: audio_apps::clip_label(&app.label) });
    Some(out)
}

fn to_json(apps: &[PlayingApp]) -> Value {
    Value::Array(apps.iter().map(|a| json!({ "exe": a.exe, "label": a.label })).collect())
}

impl Root {
    fn app_audio(&self) -> Vec<PlayingApp> {
        saved_apps(self.config_value("recording", "app_audio"))
    }

    fn save_app_audio(&mut self, apps: &[PlayingApp]) {
        self.set_config("recording", "app_audio", to_json(apps));
    }

    fn add_app(&mut self, app: PlayingApp) {
        if let Some(apps) = with_app(&self.app_audio(), app) {
            self.save_app_audio(&apps);
        }
    }

    /// Open the "Add app…" menu and list the apps playing sound now (off the UI thread).
    fn open_app_menu(&mut self, cx: &mut Context<Self>) {
        self.settings_ui.app_menu = true;
        self.settings_ui.playing_apps = PlayingApps::Loading;
        cx.spawn(async move |this, cx| {
            let apps = cx.background_executor().spawn(async { audio_apps::list_blocking() }).await;
            let _ = this.update(cx, |this, cx| {
                this.settings_ui.playing_apps = PlayingApps::Ready(apps);
                cx.notify();
            });
        })
        .detach();
    }

    fn browse_for_app(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(SharedString::from("Record this app")) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))) else { return };
            let app = cx.background_executor().spawn(async move { audio_apps::app_from_path(&path) }).await;
            let _ = this.update(cx, |this, cx| {
                this.add_app(app);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn render_app_audio(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let apps = self.app_audio();
        let full = apps.len() >= APPS_MAX;
        let mut list = div().w(px(340.)).flex().flex_col().gap(px(6.));
        for (i, app) in apps.iter().enumerate() {
            let remaining: Vec<PlayingApp> = apps.iter().enumerate().filter(|(n, _)| *n != i).map(|(_, a)| a.clone()).collect();
            list = list.child(
                div()
                    .h(px(32.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(c(theme::BORDER))
                    .child(div().flex_1().min_w_0().truncate().child(app.label.clone()))
                    .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(app.exe.clone()))
                    .child(ui::icon_button(("app-audio-remove", i), Icon::X, 24., theme::MUTED, false).child(crate::trace::target(format!("app-audio-remove-{i}"))).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.save_app_audio(&remaining);
                            cx.notify();
                        },
                    ))),
            );
        }
        let open = self.settings_ui.app_menu;
        let buttons = div()
            .relative()
            .flex()
            .gap(px(8.))
            .when(full, |d| d.opacity(0.4))
            .child(ui::button("app-audio-add", Variant::Ghost).border_1().border_color(c(theme::BORDER)).child(crate::trace::target("app-audio-add")).child("Add app…").on_click(cx.listener(
                move |this, _, _, cx| {
                    if this.settings_ui.app_menu {
                        this.settings_ui.app_menu = false;
                    } else if !full {
                        this.open_app_menu(cx);
                    }
                    cx.notify();
                },
            )))
            .child(ui::button("app-audio-browse", Variant::Ghost).border_1().border_color(c(theme::BORDER)).child("Browse…").on_click(cx.listener(move |this, _, _, cx| {
                if !full {
                    this.browse_for_app(cx);
                }
            })))
            .when(open, |d| d.child(self.app_menu(&apps, cx)));
        setting_row("App audio", "Each app here is recorded on a track of its own, even while it is closed. It is still heard in Desktop audio too.")
            .child(list.child(buttons))
            .into_any_element()
    }

    fn app_menu(&self, added: &[PlayingApp], cx: &mut Context<Self>) -> AnyElement {
        let mut panel = div()
            .id("app-audio-menu")
            .occlude()
            .absolute()
            .top(px(38.))
            .left_0()
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
                this.settings_ui.app_menu = false;
                cx.notify();
            }));
        let note = |text: &str| div().min_h(px(30.)).px(px(10.)).py(px(6.)).flex().items_center().text_color(c(theme::TERTIARY)).child(text.to_owned());
        match &self.settings_ui.playing_apps {
            PlayingApps::Ready(apps) => {
                let new: Vec<PlayingApp> = apps.iter().filter(|a| !added.iter().any(|b| b.exe.eq_ignore_ascii_case(&a.exe))).cloned().collect();
                if new.is_empty() {
                    panel = panel.child(note("No other app is playing sound. Start the app and open this again, or use Browse."));
                }
                for (i, app) in new.into_iter().enumerate() {
                    let label = app.label.clone();
                    let exe = app.exe.clone();
                    panel = panel.child(
                        div()
                            .id(("app-audio-item", i))
                            .h(px(30.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(5.))
                            .cursor_pointer()
                            .hover(|s| s.bg(c(theme::TOP)))
                            .child(icon(Icon::Volume, 14., theme::MUTED))
                            .child(div().flex_1().min_w_0().truncate().child(label))
                            .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(exe))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.settings_ui.app_menu = false;
                                this.add_app(app.clone());
                                cx.notify();
                            })),
                    );
                }
            }
            _ => panel = panel.child(note("Looking for apps playing sound…")),
        }
        deferred(panel).with_priority(1).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(exe: &str, label: &str) -> PlayingApp {
        PlayingApp { exe: exe.into(), label: label.into() }
    }

    #[test]
    fn an_app_is_added_once_and_the_list_stops_at_six() {
        let one = with_app(&[], app("a.exe", "Source A")).unwrap();
        assert_eq!(one, vec![app("a.exe", "Source A")]);
        assert_eq!(with_app(&one, app("A.EXE", "again")), None, "same exe, any case");
        let six: Vec<_> = (0..6).map(|n| app(&format!("app{n}.exe"), "")).collect();
        assert_eq!(with_app(&six, app("more.exe", "")), None);
    }

    #[test]
    fn the_saved_list_reads_back_with_names_and_writes_in_order() {
        let saved = json!([{ "exe": "a.exe", "label": "Source A" }, { "exe": "b.exe" }]);
        let apps = saved_apps(Some(&saved));
        assert_eq!(apps, vec![app("a.exe", "Source A"), app("b.exe", "b")], "no label: the exe name");
        assert_eq!(to_json(&apps[..1]), json!([{ "exe": "a.exe", "label": "Source A" }]));
        assert!(saved_apps(None).is_empty());
    }
}
