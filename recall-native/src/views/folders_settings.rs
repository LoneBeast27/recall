//! Settings → Media → Clip folders: the recordings folder plus folders of old clips the user
//! imported from other recorders. Imported folders are only listed; nothing is moved.
use std::path::PathBuf;

use gpui::{AnyElement, Context, Div, FontWeight, PathPromptOptions, SharedString, div, prelude::*, px};
use serde_json::{Value, json};

use crate::{
    root::{Root, ToastKind},
    services::library::folders::Folders,
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    views::pages::section_title,
    worker::Job,
};

impl Root {
    /// The recorder's output folder (live config, else the last one it reported).
    pub fn recordings_folder(&self) -> String {
        self.config_value("recording", "output_dir")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| std::env::var("RECALL_OUT_DIR").ok())
            .or_else(|| self.folders.recordings.clone())
            .unwrap_or_else(|| crate::services::library::folders::default_recordings_dir().to_string_lossy().into_owned())
    }

    /// Keeps `folders.json` in step with the recorder's output folder; re-lists when it moved.
    pub fn sync_recordings_folder(&mut self) {
        let Some(dir) = self.config_value("recording", "output_dir").and_then(Value::as_str).map(str::to_owned) else { return };
        if self.folders.recordings.as_deref() == Some(dir.as_str()) {
            return;
        }
        let before = crate::backend::library_dir();
        // A recorder pointed into the cache (by hand, or before this check existed) blocks it.
        if let Some(message) = crate::services::storage::guard::check(&crate::services::storage::guard::cache_dirs(), &[PathBuf::from(&dir)]) {
            self.show_toast_for(ToastKind::Bad, "Cache and clip folders overlap", Some(message), 30);
        }
        self.folders.recordings = Some(dir);
        let _ = self.folders.save();
        if crate::backend::library_dir() != before {
            self.workers.job(Job::List);
        }
    }

    pub fn prompt_clip_folders(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some(SharedString::from("Add folder")),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = this.update(cx, |this, cx| {
                this.add_clip_folders(paths);
                cx.notify();
            });
        })
        .detach();
    }

    /// Settings → Media → Recordings → Change: pick the folder new clips save to.
    pub fn prompt_recordings_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(SharedString::from("Save clips here")),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| {
                this.change_recordings_folder(dir);
                cx.notify();
            });
        })
        .detach();
    }

    /// Point the recorder at `dir`. Clips already saved stay where they are: the old folder
    /// joins the library's imported folders so they keep showing.
    fn change_recordings_folder(&mut self, dir: PathBuf) {
        if !self.snap.connected {
            self.show_toast(ToastKind::Bad, "The recorder is off", Some("Start it, then change the folder.".into()));
            return;
        }
        if let Some(why) = crate::services::storage::guard::refuse_clip_folder(&dir) {
            self.show_toast_for(ToastKind::Bad, "Can't save clips there", Some(why), 10);
            return;
        }
        let new = dir.to_string_lossy().into_owned();
        let old = self.recordings_folder();
        if same_folder(&new, &old) {
            self.show_toast(ToastKind::Info, "Already your recordings folder", None);
            return;
        }
        self.set_config("recording", "output_dir", json!(new));
        let mut folders = Folders::load();
        folders.remove(&new);
        folders.recordings = Some(new);
        let kept_old = folders.add(std::path::Path::new(&old));
        match folders.save() {
            Ok(()) => {
                self.folders = folders;
                crate::services::library::folders::forget_scans();
                self.workers.job(Job::List);
                let detail = if kept_old { "New clips save here. Clips already saved stay in the old folder and still show." } else { "New clips save here." };
                self.show_toast(ToastKind::Good, "Recordings folder changed", Some(detail.into()));
            }
            Err(error) => self.show_toast(ToastKind::Bad, "Couldn't update the library folders", Some(error)),
        }
    }

    fn add_clip_folders(&mut self, paths: Vec<PathBuf>) {
        if let Some(why) = paths.iter().find_map(|path| crate::services::storage::guard::refuse_clip_folder(path)) {
            self.show_toast_for(ToastKind::Bad, "Can't add that folder", Some(why), 10);
            return;
        }
        let mut folders = Folders::load();
        folders.recordings = self.folders.recordings.clone().or_else(|| Some(self.recordings_folder()));
        let added = paths.iter().filter(|p| folders.add(p)).count();
        // Clips removed from Recall while the folder was listed before come back with it.
        for path in &paths {
            let _ = crate::services::library::imports::unhide_under(&path.to_string_lossy());
        }
        if added == 0 {
            self.show_toast(ToastKind::Info, "Already in your library", None);
            return;
        }
        match folders.save() {
            Ok(()) => {
                self.folders = folders;
                crate::services::library::folders::forget_scans();
                self.workers.job(Job::List);
                let title = if added == 1 { "Folder added".to_owned() } else { format!("{added} folders added") };
                self.show_toast(ToastKind::Good, title, Some("Its clips appear in your library in a moment.".into()));
            }
            Err(error) => self.show_toast(ToastKind::Bad, "Couldn't add the folder", Some(error)),
        }
    }

    fn remove_clip_folder(&mut self, folder: &str) {
        let mut folders = Folders::load();
        folders.remove(folder);
        match folders.save() {
            Ok(()) => {
                self.folders = folders;
                crate::services::library::folders::forget_scans();
                self.workers.job(Job::List);
                self.show_toast(ToastKind::Info, "Folder removed", Some("Its clips are hidden from Recall, not deleted.".into()));
            }
            Err(error) => self.show_toast(ToastKind::Bad, "Couldn't remove the folder", Some(error)),
        }
    }

    pub fn render_folder_section(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let recordings = self.recordings_folder();
        let paths: Vec<&str> = self.lib.records.iter().map(|r| r.path.as_str()).collect();
        let count = |folder: &str| clips_label(paths.iter().filter(|p| inside(p, folder)).count());

        let mut list = div().flex().flex_col();
        let change = ui::button("rec-change", Variant::Ghost).child("Change").on_click(cx.listener(|this, _, _, cx| {
            this.prompt_recordings_folder(cx);
        }));
        list = list.child(folder_row("rec", "Recordings", &recordings, count(&recordings), false, cx).child(change));
        for (i, folder) in self.folders.imported.clone().into_iter().enumerate() {
            let name = std::path::Path::new(&folder).file_name().map_or_else(|| folder.clone(), |n| n.to_string_lossy().into_owned());
            let skipped = crate::services::library::imports::skipped_under(&folder);
            let label = if skipped == 0 { count(&folder) } else { format!("{} · {skipped} skipped as edited", count(&folder)) };
            list = list.child(folder_row(("imp", i), &name, &folder, label, true, cx));
        }

        let add = div()
            .pt(px(12.))
            .flex()
            .items_center()
            .gap(px(12.))
            .child(ui::button("add-folder", Variant::Secondary).child(icon(Icon::Plus, 14., theme::TEXT)).child("Add folder").on_click(cx.listener(|this, _, _, cx| {
                this.prompt_clip_folders(cx);
            })))
            .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(c(theme::TERTIARY)).child("Bring in old clips from ShadowPlay, SteelSeries, Medal or OBS. Files stay as they are; videos made in an editor like Premiere Pro are skipped."));

        vec![section_title("Clip folders"), crate::views::pages::card().child(list).child(add).into_any_element()]
    }
}

fn folder_row(id: impl Into<gpui::ElementId> + Clone, title: &str, path: &str, count: String, removable: bool, cx: &mut Context<Root>) -> Div {
    let id: gpui::ElementId = id.into();
    let open = path.to_owned();
    let remove = path.to_owned();
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .py(px(10.))
        .border_b_1()
        .border_color(c(theme::BORDER_SUBTLE))
        .child(icon(Icon::Folder, 16., theme::MUTED))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().font_weight(FontWeight::MEDIUM).truncate().child(title.to_owned()))
                .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).truncate().child(path.to_owned())),
        )
        .child(div().flex_none().text_size(px(12.)).text_color(c(theme::MUTED)).child(count))
        .child(ui::button((SharedString::from(format!("{id}-open")), 0usize), Variant::Ghost).child("Open").on_click(cx.listener(move |_, _, _, _| {
            let _ = std::process::Command::new("explorer.exe").arg(&open).spawn();
        })))
        .when(removable, |row| {
            row.child(ui::icon_button((SharedString::from(format!("{id}-remove")), 0usize), Icon::X, 28., theme::MUTED, false).on_click(cx.listener(move |this, _, _, cx| {
                this.remove_clip_folder(&remove);
                cx.notify();
            })))
        })
}

/// `path` lies inside `folder` (case-insensitive, no allocation).
pub(crate) fn inside(path: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches(['\\', '/']);
    path.len() > folder.len()
        && path.is_char_boundary(folder.len())
        && path[..folder.len()].eq_ignore_ascii_case(folder)
        && matches!(path.as_bytes()[folder.len()], b'\\' | b'/')
}

/// Two spellings of one folder (case, trailing separator).
fn same_folder(a: &str, b: &str) -> bool {
    a.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(b.trim_end_matches(['\\', '/']))
}

fn clips_label(n: usize) -> String {
    match n {
        0 => "No clips".into(),
        1 => "1 clip".into(),
        n => format!("{} clips", group_thousands(n)),
    }
}

fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_matches_folder_contents_only() {
        assert!(inside(r"E:\Clips\a.mp4", r"e:\clips"));
        assert!(inside(r"D:\Medal\Clips\Rocket League\a.mp4", r"D:\Medal\"));
        assert!(!inside(r"E:\Clips 2\a.mp4", r"E:\Clips"));
    }

    #[test]
    fn labels_group_thousands() {
        assert_eq!(clips_label(0), "No clips");
        assert_eq!(clips_label(1), "1 clip");
        assert_eq!(clips_label(4365), "4,365 clips");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }
}
