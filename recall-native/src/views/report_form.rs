//! Settings → General → Report a problem. The person describes the problem, keeps or drops the
//! anonymous trace and a settings summary, adds screenshots or exported clips with +, and presses Save
//! report. Recall first lists every file the zip will hold (`recall_trace::report::prepare`);
//! only on their confirm does it write the zip, show it in Explorer and, if they choose,
//! open a new GitHub issue with their text filled in. They attach the zip themselves, and the
//! issue is posted under their own GitHub account. Nothing is sent by Recall.
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
const MEDIA_MAX: usize = report::MAX_MEDIA;

pub struct ReportForm {
    pub field: Option<TextField>,
    /// Added files: the file and the line shown for it (its own name stays on this PC).
    pub media: Vec<(PathBuf, String)>,
    pub trace: bool,
    /// Include the settings summary.
    pub logs: bool,
    /// The last result or refusal, in words.
    pub note: Option<String>,
    /// A report made and listed, waiting for the person's confirm.
    pub pending: Option<(report::Input, report::Prepared)>,
}

impl Default for ReportForm {
    fn default() -> Self {
        ReportForm { field: None, media: Vec::new(), trace: true, logs: true, note: None, pending: None }
    }
}

/// The settings file a report summarises (`recall_trace::report::settings`).
fn settings_file() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("recall-hdr").join("config.toml")).filter(|p| p.is_file())
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

/// A size as KB under a megabyte, else MB.
/// A size in the unit that shows it: bytes, KB under a megabyte, else MB.
fn size_text(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} bytes"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => mb(bytes),
    }
}

/// "1 file", "3 files".
fn files_text(count: usize) -> String {
    if count == 1 { "1 file".into() } else { format!("{count} files") }
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
            .border_color(c(if focused { theme::FOCUS } else { theme::BORDER }))
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
                "A GitHub issue is public and is posted under your own GitHub account, so your account name shows on it. To stay unnamed, save the zip only and send it another way. The report holds no names of games, apps, devices, folders or people unless you type them here or they show in screenshots or clips you add.",
            )
            .into_any_element(),
            // A row like the settings rows, so the box takes the column's width.
            div().flex().items_center().child(text_box.flex_1().min_w_0()).into_any_element(),
            setting_row("Include the action trace", "What Recall did and what failed, in fixed words and numbers. Never names or paths.")
                .child(ui::switch("report-trace", trace).on_click(cx.listener(move |this, _, _, cx| {
                    this.settings_ui.report.trace = !trace;
                    this.settings_ui.report.pending = None;
                    cx.notify();
                })))
                .into_any_element(),
            setting_row("Include a settings summary", "Numbers and switches only, such as frame rate, codec and whether the overlay is on. No names, folders, devices or shortcuts.")
                .child(ui::switch("report-logs", logs).on_click(cx.listener(move |this, _, _, cx| {
                    this.settings_ui.report.logs = !logs;
                    this.settings_ui.report.pending = None;
                    cx.notify();
                })))
                .into_any_element(),
        ];
        let media_detail = "PNG or JPEG screenshots and MP4 clips you exported, up to 20 MB each and 24 MB in all. File metadata such as location, dates and device names is removed and files are renamed. The picture and sound are kept as they are, so whatever they show or say stays.";
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
                            this.settings_ui.report.pending = None;
                        }
                        cx.notify();
                    })))
                    .into_any_element(),
            );
        }
        let note = self.settings_ui.report.note.clone();
        if self.settings_ui.report.pending.is_some() {
            items.extend(self.render_report_preview(cx));
        } else {
            items.push(
                setting_row("Save report", note.as_deref().unwrap_or("Lists every file the report will hold. Nothing is saved or opened until you confirm."))
                    .child(ui::button("report-save", ui::Variant::Primary).child("Save report").on_click(cx.listener(|this, _, _, cx| {
                        this.prepare_report();
                        cx.notify();
                    })))
                    .into_any_element(),
            );
        }
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
                    form.pending = None;
                }
                Err(why) => form.note = Some(why.to_string()),
            }
        }
    }

    /// The list of what the report holds, and the confirm buttons.
    fn render_report_preview(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some((_, prepared)) = self.settings_ui.report.pending.as_ref() else { return Vec::new() };
        let mut lines: Vec<AnyElement> = prepared
            .files()
            .into_iter()
            .map(|(name, bytes)| div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(format!("{name} · {}", size_text(bytes))).into_any_element())
            .collect();
        lines.insert(0, div().text_size(px(12.)).child(format!("The zip will hold {}, {} in all:", files_text(prepared.entries.len()), size_text(prepared.zip_bytes()))).into_any_element());
        let buttons = div()
            .flex()
            .gap(px(8.))
            .child(ui::button("report-confirm-github", ui::Variant::Primary).child("Save and open GitHub").on_click(cx.listener(|this, _, _, cx| {
                this.save_report(true, cx);
                cx.notify();
            })))
            .child(ui::button("report-confirm-zip", ui::Variant::Secondary).child("Save zip only").on_click(cx.listener(|this, _, _, cx| {
                this.save_report(false, cx);
                cx.notify();
            })))
            .child(ui::button("report-cancel", ui::Variant::Ghost).child("Cancel").on_click(cx.listener(|this, _, _, cx| {
                this.settings_ui.report.pending = None;
                cx.notify();
            })));
        vec![div().flex().flex_col().gap(px(4.)).children(lines).into_any_element(), buttons.into_any_element()]
    }

    /// Make the report and list it; nothing is written yet.
    fn prepare_report(&mut self) {
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
            settings: if form.logs { settings_file() } else { None },
            media: form.media.iter().map(|(p, _)| p.clone()).collect(),
        };
        if report::reports_dir().is_none() {
            form.note = Some("Reports are off in test runs.".into());
            return;
        }
        match report::prepare(&input, &report::Names::from_env()) {
            Ok(prepared) => {
                form.note = None;
                form.pending = Some((input, prepared));
            }
            Err(why) => form.note = Some(why),
        }
    }

    /// The person confirmed: write the zip, show it, and open GitHub if they chose to.
    fn save_report(&mut self, open_github: bool, cx: &mut Context<Self>) {
        let form = &mut self.settings_ui.report;
        let (Some((input, prepared)), Some(dir)) = (form.pending.take(), report::reports_dir()) else { return };
        recall_trace::act("report.saved", &[("media", input.media.len().into()), ("trace", input.trace_dir.is_some().into()), ("settings", input.settings.is_some().into()), ("github", open_github.into())]);
        match report::save(&prepared, &dir) {
            Ok(path) => {
                let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                form.note = Some(if open_github {
                    format!("Saved {name} ({}). Drag it onto the GitHub issue that opened.", size_text(prepared.zip_bytes()))
                } else {
                    format!("Saved {name} ({}). Send it the way you choose.", size_text(prepared.zip_bytes()))
                });
                crate::backend::show_in_folder(&path.to_string_lossy());
                if open_github {
                    cx.open_url(&report::issue_url(&input));
                }
            }
            Err(why) => form.note = Some(why),
        }
    }

    /// Keys for the description box; false when it doesn't have focus.
    pub(crate) fn report_input_key(&mut self, event: &KeyDownEvent, clipboard: Option<String>, window: &mut Window) -> bool {
        let Some(field) = self.settings_ui.report.field.as_mut().filter(|f| f.is_focused(window)) else { return false };
        match field.key_down(event, clipboard) {
            FieldEvent::Cancel => window.focus(&self.focus),
            // The listed report holds the old text: list it again after an edit.
            FieldEvent::Changed => self.settings_ui.report.pending = None,
            FieldEvent::Submit | FieldEvent::None => {}
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
    fn small_sizes_and_counts_read_naturally() {
        assert_eq!(super::size_text(312), "312 bytes");
        assert_eq!(super::size_text(2048), "2.0 KB");
        assert_eq!(super::size_text(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(super::files_text(1), "1 file");
        assert_eq!(super::files_text(3), "3 files");
    }

    #[test]
    fn sizes_read_in_megabytes() {
        assert_eq!(super::mb(0), "0.0 MB");
        assert_eq!(super::mb(20 * 1024 * 1024), "20.0 MB");
    }
}
