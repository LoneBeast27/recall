//! Settings → General → Report a problem. The person describes the problem, keeps or drops the
//! anonymous trace and logs, adds screenshots or exported clips with +, and saves a report zip
//! (`recall_trace::report`). Recall then shows the zip in Explorer and opens a new GitHub issue
//! with their text filled in; they attach the zip there themselves. Nothing is sent by Recall.
use std::path::PathBuf;

use gpui::{AnyElement, Context, KeyDownEvent, PathPromptOptions, SharedString, Window, div, prelude::*, px};
use recall_trace::report;

use crate::{
    root::Root,
    theme::{self, c},
    ui::{self, FieldEvent, TextField},
    views::pages::{section_title, setting_row},
};

/// The longest description the box takes (the issue address holds about 8 KB).
const TEXT_MAX: usize = 1500;
/// Media a report takes at most.
const MEDIA_MAX: usize = 6;

pub struct ReportForm {
    pub field: Option<TextField>,
    /// Added files: the file and the line shown for it (its own name stays on this PC).
    pub media: Vec<(PathBuf, String)>,
    pub trace: bool,
    pub logs: bool,
    /// The last result or refusal, in words.
    pub note: Option<String>,
}

impl Default for ReportForm {
    fn default() -> Self {
        ReportForm { field: None, media: Vec::new(), trace: true, logs: true, note: None }
    }
}

/// Developer logs and settings a report carries as redacted copies: (name in the report, file).
fn log_files() -> Vec<(String, PathBuf)> {
    let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) else { return Vec::new() };
    let (hdr, app) = (roaming.join("recall-hdr"), roaming.join("Recall"));
    [
        ("recorder.log", hdr.join("recall-hdr.log")),
        ("recorder.previous.log", hdr.join("recall-hdr.log.1")),
        ("island.log", hdr.join("recall-overlay.log")),
        ("island.previous.log", hdr.join("recall-overlay.log.old")),
        ("library.log", app.join("player.log")),
        ("library.previous.log", app.join("player.old.log")),
        ("settings.toml", hdr.join("config.toml")),
    ]
    .into_iter()
    .filter(|(_, path)| path.is_file())
    .map(|(name, path)| (name.to_string(), path))
    .collect()
}

/// The Windows build number (`CurrentBuild`), or "unknown".
fn windows_build() -> String {
    use windows::{
        Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
        core::w,
    };
    let mut buf = [0u16; 32];
    let mut len = (buf.len() * 2) as u32;
    let read = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"), w!("CurrentBuild"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut len))
    };
    if read.is_err() {
        return "unknown".into();
    }
    let text = String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1).min(buf.len())]);
    if text.chars().all(|ch| ch.is_ascii_digit()) && !text.is_empty() { text } else { "unknown".into() }
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

impl Root {
    /// The Report a problem section.
    pub(crate) fn settings_report(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let form = &mut self.settings_ui.report;
        let field = form.field.get_or_insert_with(|| TextField::new(cx, "What went wrong, and what were you doing?", TEXT_MAX));
        let focused = field.is_focused(window);
        let text_box = div()
            .w_full()
            .min_h(px(36.))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(6.))
            .bg(c(theme::RAISED))
            .border_1()
            .border_color(c(if focused { theme::BORDER_STRONG } else { theme::BORDER }))
            .child(crate::trace::target("report-text"))
            .child(field.render("report-text", window).on_click(cx.listener(|this, _, window, cx| {
                if let Some(field) = this.settings_ui.report.field.as_ref() {
                    window.focus(&field.focus);
                }
                cx.notify();
            })));
        let (trace, logs) = (form.trace, form.logs);
        let mut items = vec![
            section_title("Report a problem"),
            setting_row(
                "Describe the problem",
                "Your report goes on a public GitHub issue. Recall removes your Windows user name, PC name, folder paths, and app, clip and device names from the logs it adds. Your own words are sent as you type them.",
            )
            .into_any_element(),
            // A row like the settings rows, so the box takes the column's width.
            div().flex().items_center().child(text_box.flex_1().min_w_0()).into_any_element(),
            setting_row("Include the action trace", "What Recall did and what failed, in fixed words and numbers. Never names or paths.")
                .child(ui::switch("report-trace", trace).on_click(cx.listener(move |this, _, _, cx| {
                    this.settings_ui.report.trace = !trace;
                    cx.notify();
                })))
                .into_any_element(),
            setting_row("Include anonymous logs and settings", "Copies of Recall's logs and settings with names and paths replaced.")
                .child(ui::switch("report-logs", logs).on_click(cx.listener(move |this, _, _, cx| {
                    this.settings_ui.report.logs = !logs;
                    cx.notify();
                })))
                .into_any_element(),
        ];
        let media_detail = "PNG or JPEG screenshots and MP4 clips you exported, up to 20 MB each and 24 MB in all. Picture details such as location are removed, and files are renamed.";
        let mut add = ui::button("report-add-media", ui::Variant::Secondary).child("+ Add media");
        if self.settings_ui.report.media.len() < MEDIA_MAX {
            add = add.on_click(cx.listener(|this, _, _, cx| this.prompt_report_media(cx)));
        }
        items.push(setting_row("Screenshots and clips", media_detail).child(add).into_any_element());
        for (i, (_, line)) in self.settings_ui.report.media.iter().enumerate() {
            items.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(line.clone()))
                    .child(ui::button(("report-media-remove", i), ui::Variant::Secondary).child("Remove").on_click(cx.listener(move |this, _, _, cx| {
                        if i < this.settings_ui.report.media.len() {
                            this.settings_ui.report.media.remove(i);
                        }
                        cx.notify();
                    })))
                    .into_any_element(),
            );
        }
        let note = self.settings_ui.report.note.clone();
        items.push(
            setting_row("Save report and open GitHub", note.as_deref().unwrap_or("Saves a zip you can look through, shows it in Explorer, then opens a new issue. Drag the zip onto the issue."))
                .child(ui::button("report-save", ui::Variant::Primary).child("Save report").on_click(cx.listener(|this, _, _, cx| {
                    this.save_report(cx);
                    cx.notify();
                })))
                .into_any_element(),
        );
        items.push(
            setting_row("Clear the action trace", "Removes the trace and crash records kept on this PC. Clips and settings stay.")
                .child(ui::button("report-clear", ui::Variant::Secondary).child("Clear").on_click(cx.listener(|this, _, _, cx| {
                    let removed = recall_trace::run_dir().map_or(0, |dir| report::clear_trace(&dir));
                    this.settings_ui.report.note = Some(format!("Removed {removed} trace and crash files."));
                    cx.notify();
                })))
                .into_any_element(),
        );
        items
    }

    fn prompt_report_media(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(SharedString::from("Add to the report")) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = this.update(cx, |this, cx| {
                this.add_report_media(paths);
                cx.notify();
            });
        })
        .detach();
    }

    /// Take the files the + button picked that are allowed; say why any were refused.
    pub(crate) fn add_report_media(&mut self, paths: Vec<PathBuf>) {
        let form = &mut self.settings_ui.report;
        form.note = None;
        for path in paths {
            if form.media.len() >= MEDIA_MAX {
                form.note = Some(format!("A report takes up to {MEDIA_MAX} files."));
                break;
            }
            if form.media.iter().any(|(p, _)| *p == path) {
                continue;
            }
            match report::check_media(&path) {
                Ok(kind) => {
                    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
                    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    form.media.push((path, format!("{name} · {} · {}", kind.ext().to_uppercase(), mb(size))));
                }
                Err(why) => form.note = Some(why.to_string()),
            }
        }
    }

    fn save_report(&mut self, cx: &mut Context<Self>) {
        let form = &mut self.settings_ui.report;
        let text = form.field.as_ref().map(|f| f.value.trim().to_string()).unwrap_or_default();
        if text.is_empty() {
            form.note = Some("Describe the problem first.".into());
            return;
        }
        let input = report::Input {
            text,
            version: crate::update::current().unwrap_or("development build").to_string(),
            windows: windows_build(),
            trace_dir: form.trace.then(recall_trace::run_dir).flatten(),
            logs: if form.logs { log_files() } else { Vec::new() },
            media: form.media.iter().map(|(p, _)| p.clone()).collect(),
        };
        let Some(dir) = report::reports_dir() else {
            form.note = Some("Recall could not find a folder for reports.".into());
            return;
        };
        recall_trace::act("report.saved", &[("media", input.media.len().into()), ("trace", (input.trace_dir.is_some()).into()), ("logs", (!input.logs.is_empty()).into())]);
        match report::build(&input, &dir, &report::Names::from_env()) {
            Ok(built) => {
                let name = built.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                form.note = Some(format!("Saved {name} ({}, {} files). Drag it onto the GitHub issue that opened.", mb(built.bytes), built.files.len()));
                crate::backend::show_in_folder(&built.path.to_string_lossy());
                cx.open_url(&report::issue_url(&input));
            }
            Err(why) => form.note = Some(why),
        }
    }

    /// Keys for the description box; false when it doesn't have focus.
    pub(crate) fn report_input_key(&mut self, event: &KeyDownEvent, clipboard: Option<String>, window: &mut Window) -> bool {
        let Some(field) = self.settings_ui.report.field.as_mut().filter(|f| f.is_focused(window)) else { return false };
        if let FieldEvent::Cancel = field.key_down(event, clipboard) {
            window.focus(&self.focus);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_windows_build_is_a_number_or_unknown() {
        let build = super::windows_build();
        assert!(build == "unknown" || (build.parse::<u32>().is_ok_and(|n| n >= 10_000)), "{build}");
    }

    #[test]
    fn sizes_read_in_megabytes() {
        assert_eq!(super::mb(0), "0.0 MB");
        assert_eq!(super::mb(20 * 1024 * 1024), "20.0 MB");
    }
}
