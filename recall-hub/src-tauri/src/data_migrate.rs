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

/// Copies `<base>\OLD_IDENTIFIER` to `<base>\NEW_IDENTIFIER` for the roaming and
/// local app-data bases, where the new folder is missing. Best effort: a failure
/// is logged and the hub starts with whatever is there.
pub fn migrate_app_data() {
    for var in ["APPDATA", "LOCALAPPDATA"] {
        let Some(base) = std::env::var_os(var).map(PathBuf::from) else { continue };
        if let Err(e) = migrate_in(&base, OLD_IDENTIFIER, NEW_IDENTIFIER) {
            eprintln!("recall-hub: app data carry-over in %{var}% failed: {e}");
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
