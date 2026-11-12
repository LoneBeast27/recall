//! The start after an update (`--resume <file>`, written by `update_resume.rs` and completed by
//! Setup): settings that no longer parse are restored from Setup's backup (`update_restore.rs`),
//! the page and the clip come back, and the outcome is said once: "Recall is updated", "Update
//! failed, still on vX", "The last recording was cut short during the update and was recovered".
use std::path::Path;

use crate::{
    root::{Root, Route, ToastKind},
    update::{Resume, resume},
};

/// The page a resume file names (`Route::name`).
pub fn route_named(name: &str) -> Option<Route> {
    [Route::Library, Route::Editor, Route::Hdr, Route::Audio, Route::Settings, Route::Report].into_iter().find(|r| r.name() == name)
}

impl Root {
    /// `--resume <file>` on the command line: reopen where the update left off.
    pub(super) fn resume_after_update(&mut self, file: &Path) {
        let Some(updates) = resume::updates_dir() else { return };
        let Some(state) = Resume::read(file, &updates) else {
            crate::trace::player_log("update resume", serde_json::json!({ "ignored": file.display().to_string() }));
            return;
        };
        // Read once: a later start with the same argument does nothing.
        let _ = std::fs::remove_file(file);
        resume::sweep_downloads(&updates);
        self.update.after_update(state.expect_revision.as_deref());
        let mut notes = state.notices();
        // Files restored from Setup's backup before the window opened (entry.rs).
        notes.extend(crate::update_restore::take_notes().into_iter().map(|n| (false, n)));
        match (state.clip.clone(), route_named(&state.route)) {
            (Some(clip), Some(Route::Editor)) if Path::new(&clip).exists() => self.open_clip(clip),
            (_, Some(route)) if route != Route::Editor => self.go(route),
            _ => {}
        }
        crate::trace::player_log("update resume", serde_json::json!({ "outcome": state.outcome, "route": state.route, "notes": notes.len() }));
        let Some((good, first)) = notes.first().cloned() else { return };
        let detail = (notes.len() > 1).then(|| notes[1..].iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n"));
        let bad = !good || notes.iter().any(|(g, _)| !g);
        self.show_toast_for(if bad { ToastKind::Bad } else { ToastKind::Good }, first, detail, 15);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_name_maps_back() {
        for r in [Route::Library, Route::Editor, Route::Hdr, Route::Audio, Route::Settings, Route::Report] {
            assert_eq!(route_named(r.name()), Some(r));
        }
        assert_eq!(route_named("nowhere"), None);
    }
}
