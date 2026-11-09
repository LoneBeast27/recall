//! After an update (`--resume`), before anything is loaded: a settings file this build cannot
//! read at all is restored from the copy Recall Setup made before the update
//! (`config-backup-…`, `scripts/setup/SetupUserState.cs`), and the user is told; the damaged
//! file is kept beside it (`<name>.bad-<date>`). Never silently defaulted. config.toml is the
//! recorder's (`recall-hdr/src/config_backup.rs`, per value).
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// (root, path under it) of the app's own settings files in a backup (Setup's names).
const FILES: [(&str, &str); 7] = [
    ("roaming", r"Recall\library.json"),
    ("roaming", r"Recall\folders.json"),
    ("roaming", r"Recall\imports.json"),
    ("roaming", r"Recall\recorded.json"),
    ("roaming", r"Recall\editor.json"),
    ("local", r"Recall\updates\settings.json"),
    ("install", r"cache\storage.json"),
];

static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Where the backup's roots are on this PC.
pub struct Roots {
    pub roaming: PathBuf,
    pub local: PathBuf,
    pub install: PathBuf,
}

impl Roots {
    fn of(&self, key: &str) -> &Path {
        match key {
            "roaming" => &self.roaming,
            "local" => &self.local,
            _ => &self.install,
        }
    }

    pub fn here() -> Option<Self> {
        let roaming = crate::user_dirs::appdata()?;
        let local = crate::user_dirs::local_appdata()?;
        let install = crate::update::install_dir().ok()?;
        Some(Self { roaming, local, install })
    }
}

fn readable(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|v| v.is_object())
}

/// Restores every file of `backup` whose live copy is there but not readable JSON (or missing
/// while the backup has it). Returns one note per file restored.
pub fn restore_from(backup: &Path, roots: &Roots, now: std::time::SystemTime) -> Vec<String> {
    let mut notes = Vec::new();
    for (key, rel) in FILES {
        let saved = backup.join(key).join(rel);
        let Ok(copy) = std::fs::read(&saved) else { continue };
        if !readable(&copy) {
            continue;
        }
        let live = roots.of(key).join(rel);
        let current = std::fs::read(&live);
        if current.as_deref().is_ok_and(readable) {
            continue;
        }
        if current.is_ok() {
            let _ = crate::services::durable::quarantine(&live, now, crate::services::durable::Keep::Move);
        }
        let name = live.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match crate::services::durable::write_durable(&live, &copy) {
            Ok(()) => notes.push(format!("{name} could not be read after the update; restored from the settings backup made before it")),
            Err(e) => notes.push(format!("{name} could not be read after the update, and restoring it failed: {e}")),
        }
    }
    notes
}

/// At start, with `--resume`: restore from the backup the resume file names; the notes wait
/// for the window (`take_notes`).
pub fn on_resume_start(resume_file: &Path) {
    let Some(updates) = crate::update::resume::updates_dir() else { return };
    let Some(state) = crate::update::Resume::read(resume_file, &updates) else { return };
    let (Some(backup), Some(roots)) = (state.config_backup.as_deref(), Roots::here()) else { return };
    let Some(backup) = backup_in(Path::new(backup), &updates) else {
        crate::trace::player_log("update restore", serde_json::json!({ "refused": backup }));
        return;
    };
    let notes = restore_from(&backup, &roots, std::time::SystemTime::now());
    if let Ok(mut all) = NOTES.lock() {
        all.extend(notes);
    }
}

/// The backup the resume file names, only when it is a `config-backup-*` folder directly in the
/// updates folder (the file is written by Setup, but read here: never a path elsewhere).
pub fn backup_in(backup: &Path, updates: &Path) -> Option<PathBuf> {
    let name = backup.file_name()?.to_string_lossy().into_owned();
    if !name.starts_with("config-backup-") {
        return None;
    }
    let (dir, parent) = (std::fs::canonicalize(backup).ok()?, std::fs::canonicalize(updates).ok()?);
    (dir.parent() == Some(parent.as_path()) && dir.is_dir()).then_some(dir)
}

/// The restore notes, once.
pub fn take_notes() -> Vec<String> {
    NOTES.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_does_not_read_is_restored_from_the_backup_with_a_notice() {
        let root = tempfile::tempdir().unwrap();
        let roots = Roots { roaming: root.path().join("Roaming"), local: root.path().join("Local"), install: root.path().join("install") };
        let backup = root.path().join("config-backup-0.1.0-20261110-120000");
        let put = |base: &Path, rel: &str, text: &str| {
            let p = base.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        put(&backup.join("roaming"), r"Recall\library.json", r#"{"D:\\a.mp4":{"favorite":true}}"#);
        put(&backup.join("roaming"), r"Recall\folders.json", r#"{"recordings":"D:\\Clips"}"#);
        put(&backup.join("install"), r"cache\storage.json", r#"{"cache_budget_bytes":5}"#);
        put(&roots.roaming, r"Recall\library.json", "\u{0}\u{0}\u{0}");
        put(&roots.roaming, r"Recall\folders.json", r#"{"recordings":"E:\\New"}"#);
        let notes = restore_from(&backup, &roots, std::time::SystemTime::now());
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].starts_with("library.json could not be read after the update; restored"));
        assert_eq!(std::fs::read_to_string(roots.roaming.join(r"Recall\library.json")).unwrap(), r#"{"D:\\a.mp4":{"favorite":true}}"#);
        assert_eq!(std::fs::read_to_string(roots.roaming.join(r"Recall\folders.json")).unwrap(), r#"{"recordings":"E:\\New"}"#, "a readable file is never replaced");
        assert!(roots.install.join(r"cache\storage.json").exists(), "a missing file comes back");
        let kept = std::fs::read_dir(roots.roaming.join("Recall")).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("library.json.bad-"));
        assert!(kept, "the damaged file is kept");
        assert!(restore_from(&backup, &roots, std::time::SystemTime::now()).is_empty(), "nothing left to restore");
    }

    #[test]
    fn only_a_config_backup_in_the_updates_folder_is_used() {
        let root = tempfile::tempdir().unwrap();
        let updates = root.path().join("updates");
        let good = updates.join("config-backup-0.1.0-20261110-120000");
        let other = root.path().join("config-backup-0.1.0-20261110-120000");
        let named = updates.join("download");
        for d in [&good, &other, &named] {
            std::fs::create_dir_all(d).unwrap();
        }
        assert_eq!(backup_in(&good, &updates), Some(std::fs::canonicalize(&good).unwrap()));
        assert_eq!(backup_in(&other, &updates), None, "outside the updates folder");
        assert_eq!(backup_in(&named, &updates), None, "not a config backup");
        assert_eq!(backup_in(&updates.join("config-backup-x").join(".."), &updates), None, "no dot tricks");
    }
}
