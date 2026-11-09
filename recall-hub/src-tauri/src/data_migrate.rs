//! One-time carry-over of the hub's data folders from its old bundle identifier.
//!
//! Tauri names the app data folders after `identifier` in tauri.conf.json
//! (`%APPDATA%\<id>` and `%LOCALAPPDATA%\<id>`, the latter holding the WebView2
//! profile). When the identifier changed, the old folders would be orphaned, so at
//! start-up each new folder that is missing is filled with a copy of the old one.
//! The old folder is never moved or deleted. Runs before the window (and its
//! WebView) exists, so nothing holds the new folder open.

use std::path::{Path, PathBuf};

pub const OLD_IDENTIFIER: &str = "com.rohan.recall-hub";
pub const NEW_IDENTIFIER: &str = "io.github.lonebeast27.recall-hub";

/// MIGRATION ONLY, never a default: the clip folder the hub used to hard-code before
/// it had a saved setting. A hub that ran under OLD_IDENTIFIER, has no saved clip
/// folder and finds this folder on disk gets it saved once (hub_settings), so its
/// clips page does not move. Everyone else falls back to <Videos>\Recall.
const LEGACY_CLIP_FOLDER: &str = r"V:\ALL RECORDINGS";

/// Copies `<base>\OLD_IDENTIFIER` to `<base>\NEW_IDENTIFIER` for the roaming and
/// local app-data bases, where the new folder is missing, then carries the legacy
/// clip folder over. Best effort: a failure is logged and the hub starts with
/// whatever is there.
pub fn migrate_app_data() {
    for var in ["APPDATA", "LOCALAPPDATA"] {
        let Some(base) = std::env::var_os(var).map(PathBuf::from) else { continue };
        if let Err(e) = migrate_in(&base, OLD_IDENTIFIER, NEW_IDENTIFIER) {
            eprintln!("recall-hub: app data carry-over in %{var}% failed: {e}");
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        let ran_before = local.join(OLD_IDENTIFIER).is_dir();
        let settings = crate::hub_settings::settings_path(&local.join(NEW_IDENTIFIER));
        carry_legacy_clip_folder(&settings, ran_before, LEGACY_CLIP_FOLDER, Path::new(LEGACY_CLIP_FOLDER).is_dir());
    }
}

/// Saves `legacy` as the hub's clip folder when [`crate::hub_settings::legacy_folder_to_save`] says so.
fn carry_legacy_clip_folder(settings: &Path, ran_before: bool, legacy: &str, legacy_exists: bool) {
    let saved = crate::hub_settings::saved_clip_folder(settings);
    if let Some(folder) = crate::hub_settings::legacy_folder_to_save(saved.as_deref(), ran_before, legacy, legacy_exists) {
        if let Err(e) = crate::hub_settings::save_clip_folder(settings, folder) {
            eprintln!("recall-hub: saving the clip folder failed: {e}");
        }
    }
}

/// Copies `base/old` to `base/new` when `new` is missing and `old` is a folder.
/// Copies into a temporary sibling first and renames it into place, so a copy cut
/// short never looks finished. Returns whether a copy happened.
pub fn migrate_in(base: &Path, old: &str, new: &str) -> std::io::Result<bool> {
    let (from, to) = (base.join(old), base.join(new));
    if to.exists() || !from.is_dir() {
        return Ok(false);
    }
    let tmp = base.join(format!("{new}.migrating-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let copied = copy_dir(&from, &tmp).and_then(|()| std::fs::rename(&tmp, &to));
    if copied.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    copied.map(|()| true)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &dest)?;
        }
        // Symlinks and other entries are skipped: the hub never writes them.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("recall-hub-migrate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn legacy_clip_folder_is_saved_once_and_never_over_a_setting() {
        use crate::hub_settings::{save_clip_folder, saved_clip_folder, settings_path};
        let base = scratch("legacy");
        let settings = settings_path(&base);
        carry_legacy_clip_folder(&settings, true, r"Z:\Old", false);
        assert!(!settings.exists(), "legacy folder missing -> no write");
        carry_legacy_clip_folder(&settings, true, r"Z:\Old", true);
        assert_eq!(saved_clip_folder(&settings).as_deref(), Some(r"Z:\Old"), "no setting + legacy exists -> saved");
        save_clip_folder(&settings, r"D:\Mine").unwrap();
        carry_legacy_clip_folder(&settings, true, r"Z:\Old", true);
        assert_eq!(saved_clip_folder(&settings).as_deref(), Some(r"D:\Mine"), "saved setting untouched");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn copies_old_folder_and_keeps_it() {
        let base = scratch("copy");
        std::fs::create_dir_all(base.join("old/EBWebView/Default")).unwrap();
        std::fs::write(base.join("old/EBWebView/Default/Prefs"), b"x").unwrap();
        assert!(migrate_in(&base, "old", "new").unwrap());
        assert_eq!(std::fs::read(base.join("new/EBWebView/Default/Prefs")).unwrap(), b"x");
        assert!(base.join("old/EBWebView/Default/Prefs").exists(), "old folder stays");
        assert!(!migrate_in(&base, "old", "new").unwrap(), "second start copies nothing");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn leaves_an_existing_new_folder_alone() {
        let base = scratch("keep");
        std::fs::create_dir_all(base.join("old")).unwrap();
        std::fs::write(base.join("old/a"), b"old").unwrap();
        std::fs::create_dir_all(base.join("new")).unwrap();
        std::fs::write(base.join("new/a"), b"new").unwrap();
        assert!(!migrate_in(&base, "old", "new").unwrap());
        assert_eq!(std::fs::read(base.join("new/a")).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn nothing_to_do_without_an_old_folder() {
        let base = scratch("none");
        assert!(!migrate_in(&base, "old", "new").unwrap());
        assert!(!base.join("new").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
