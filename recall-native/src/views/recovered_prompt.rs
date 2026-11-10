//! "<video name> (recovered)": the recorder recovered a long recording that Recall closed
//! before it finished (`recording_restored`: repaired to its last whole fragment and given its
//! length, or as it was when it could not be repaired). The app asks what to do with it:
//! - **Resume recording**: a new long recording that continues it as its next part
//!   (`record_resume` → the recorder's `record_start{continues}`: `__rec-2.mp4`, …). Only this
//!   click ever starts it; the recovery itself never does.
//! - **End it here**: the recording stays as it is (it is already in the library).
//!
//! Questions still open live in a small file (`%APPDATA%\Recall\recovered-prompts.json`), so
//! one that came while the app was closed shows on the next start. Text and choices are pure
//! functions below.
use std::path::{Path, PathBuf};

use gpui::{AnyElement, Context, FontWeight, div, prelude::*, px};
use serde::{Deserialize, Serialize};

use crate::{
    root::Root,
    services::{daemon::DaemonCommand, library::format_duration},
    theme::{self, c, ca},
    ui,
};

/// One recovered recording waiting for an answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub path: String,
    /// Its length after the repair (None: it could not be repaired, length unknown).
    #[serde(default)]
    pub seconds: Option<f64>,
    #[serde(default)]
    pub repaired: bool,
}

pub const RESUME: &str = "Resume recording";
pub const END: &str = "End it here";

fn name(p: &Pending) -> String {
    Path::new(&p.path).file_name().map_or_else(|| p.path.clone(), |n| n.to_string_lossy().into_owned())
}

pub fn title(p: &Pending) -> String {
    match p.repaired {
        true => format!("{} (recovered)", name(p)),
        false => format!("{} (recovered, length unknown)", name(p)),
    }
}

/// The body, one paragraph per line.
pub fn body(p: &Pending) -> Vec<String> {
    let mut lines = vec!["Recall closed before it finished. The last few seconds may be missing.".to_string()];
    match (p.repaired, p.seconds) {
        (true, Some(secs)) => lines.push(format!("Length: {}", format_duration(secs))),
        (true, None) => {}
        (false, _) => lines.push("It plays, but seeking may be slow.".to_string()),
    }
    lines.push("Resume recording continues it in a new file next to it, or end it here.".to_string());
    lines
}

/// `list` plus `p`, unless that file is already asked about.
pub fn add(list: &mut Vec<Pending>, p: Pending) -> bool {
    if list.iter().any(|q| same(&q.path, &p.path)) {
        return false;
    }
    list.push(p);
    true
}

fn same(a: &str, b: &str) -> bool {
    a.replace('/', "\\").eq_ignore_ascii_case(&b.replace('/', "\\"))
}

/// The answer for `path`: the question is closed either way; Resume recording is the one
/// command (`None` for End it here, or a file not asked about).
pub fn answer(list: &mut Vec<Pending>, path: &str, resume: bool) -> Option<DaemonCommand> {
    let before = list.len();
    list.retain(|p| !same(&p.path, path));
    (resume && list.len() != before).then(|| DaemonCommand::RecordResume { path: path.to_string() })
}

/// The open-questions file: `RECALL_RECOVERED_PROMPTS`, else beside `RECALL_LIBRARY_META` (a
/// sandboxed run's app data), else `%APPDATA%\Recall\recovered-prompts.json`. Every result
/// passes `user_dirs::checked`, so a unit test that reaches the real profile panics.
pub(crate) fn prompts_path_from(own: Option<std::ffi::OsString>, library: Option<std::ffi::OsString>, appdata: Option<PathBuf>) -> Option<PathBuf> {
    const FILE: &str = "recovered-prompts.json";
    let path = if let Some(own) = own.filter(|p| !p.is_empty()) {
        PathBuf::from(own)
    } else if let Some(library) = library.filter(|p| !p.is_empty()) {
        PathBuf::from(library).with_file_name(FILE)
    } else {
        appdata?.join("Recall").join(FILE)
    };
    Some(crate::user_dirs::checked(path))
}

fn prompts_path() -> Option<PathBuf> {
    prompts_path_from(std::env::var_os("RECALL_RECOVERED_PROMPTS"), std::env::var_os("RECALL_LIBRARY_META"), crate::user_dirs::appdata())
}

pub fn load_from(path: &Path) -> Vec<Pending> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_to(path: &Path, list: &[Pending]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?;
    crate::services::durable::write_durable(path, &bytes)
}

/// At start: the questions left open whose file is still there.
pub fn load_open() -> Vec<Pending> {
    let mut list = prompts_path().map(|p| load_from(&p)).unwrap_or_default();
    list.retain(|p| Path::new(&p.path).is_file());
    list
}

fn save(list: &[Pending]) {
    if let Some(Err(e)) = prompts_path().map(|path| save_to(&path, list)) {
        eprintln!("recovered recordings: open questions not saved ({e})");
    }
}

impl Root {
    /// `recording_restored` from the recorder: ask (never start anything here).
    pub fn on_recording_restored(&mut self, path: String, seconds: Option<f64>, repaired: bool) {
        if add(&mut self.recovered_prompts, Pending { path, seconds, repaired }) {
            save(&self.recovered_prompts);
        }
    }

    fn answer_recovered(&mut self, path: &str, resume: bool) {
        let command = answer(&mut self.recovered_prompts, path, resume);
        save(&self.recovered_prompts);
        if let Some(command) = command {
            self.send(command);
        }
    }

    pub fn render_recovered_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.recovered_prompts.first()?.clone();
        let (resume, end) = (p.path.clone(), p.path.clone());
        let actions = div()
            .flex()
            .gap(px(8.))
            .justify_end()
            .child(ui::button("recovered-end", ui::Variant::Secondary).child(END).on_click(cx.listener(move |this, _, _, cx| {
                this.answer_recovered(&end, false);
                cx.notify();
            })))
            .child(ui::button("recovered-resume", ui::Variant::Primary).child(RESUME).on_click(cx.listener(move |this, _, _, cx| {
                this.answer_recovered(&resume, true);
                cx.notify();
            })));
        let mut card = ui::panel()
            .w(px(480.))
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .bg(c(theme::RAISED))
            .border_color(c(theme::BORDER))
            .child(div().font_family(theme::display()).text_size(px(18.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD).child(title(&p)));
        for line in body(&p) {
            card = card.child(div().text_color(c(theme::MUTED)).child(line));
        }
        Some(
            div()
                .id("recovered-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(ca(0x000000, 0.55))
                // The scrim takes the press so the page under it does not act.
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(card.child(actions))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
#[path = "recovered_prompt_tests.rs"]
mod tests;
