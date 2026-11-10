//! The update hand-off. Before Recall hands an update to Setup and quits itself, it writes
//! `%LOCALAPPDATA%\Recall\updates\resume.json`: the page and clip it showed, the recorder's state
//! and this version. Setup (`scripts/setup/SetupUpdateFlow.cs`) reads the recorder's state, quits
//! the recorder with `save_first`, swaps the files, starts the recorder again (and its long
//! recording, if one was on) and reopens Recall with `--resume <file>`, after writing the outcome
//! into the same file: `outcome` "updated" | "failed", `still_on`, `error`, `recovered`.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "resume.json";
/// The reopened app's notices.
pub const CUT_SHORT: &str = "The last recording was cut short during the update and was recovered";

/// What the recorder was doing when the update was asked for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecorderState {
    /// The recorder was connected.
    pub running: bool,
    /// It captured a game or the desktop (the replay buffer was on).
    pub recording: bool,
    /// A long recording was running.
    pub long_on: bool,
    /// A game was being captured (a reinstall refuses then).
    pub game: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Resume {
    pub version: u32,
    /// `Route::name()` of the page shown.
    pub route: String,
    /// The clip open in the editor.
    pub clip: Option<String>,
    pub recorder: RecorderState,
    pub from_version: Option<String>,
    /// Written by Setup.
    pub outcome: Option<String>,
    pub still_on: Option<String>,
    pub error: Option<String>,
    pub recovered: Option<String>,
    pub config_backup: Option<String>,
    /// Keys a newer Setup or app added: kept as they are.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// `%LOCALAPPDATA%\Recall\updates`.
pub fn updates_dir() -> Option<PathBuf> {
    crate::user_dirs::local_appdata().map(|p| crate::user_dirs::checked(p.join("Recall").join("updates")))
}

impl Resume {
    pub fn new(route: &str, clip: Option<String>, recorder: RecorderState) -> Self {
        Self { version: 1, route: route.into(), clip, recorder, from_version: super::current().map(str::to_owned), ..Self::default() }
    }

    /// Written whole into `dir` (temp file + rename); returns its path.
    pub fn write_in(&self, dir: &Path) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        let path = dir.join(FILE);
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::services::durable::write_durable(&path, &bytes).map_err(|e| format!("Cannot save {}: {e}", path.display()))?;
        Ok(path)
    }

    /// The file `--resume` names: only `resume.json` in the updates folder (anything else is ignored).
    pub fn read(path: &Path, updates: &Path) -> Option<Self> {
        let ok = path.file_name().is_some_and(|n| n == FILE) && path.parent().is_some_and(|p| same_dir(p, updates));
        if !ok {
            return None;
        }
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// The reopened app's toasts: (good, text).
    pub fn notices(&self) -> Vec<(bool, String)> {
        let mut out = Vec::new();
        match self.outcome.as_deref() {
            Some("updated") => out.push((true, format!("Recall is updated{}", super::current().map(|v| format!(" to {v}")).unwrap_or_default()))),
            Some("failed") => {
                match self.still_on.as_deref().or(self.from_version.as_deref()) {
                    Some(on) => out.push((false, format!("Update failed, still on v{}", on.trim_start_matches('v')))),
                    None => out.push((false, "Update failed; Recall was not changed".to_owned())),
                }
            }
            _ => {}
        }
        if self.recovered.is_some() {
            out.push((false, CUT_SHORT.to_owned()));
        }
        out
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The Setup copies earlier hand-offs left in `<updates>\download` (Setup ran from them, so they
/// could not go then): removed now, best-effort; one still running stays.
pub fn sweep_downloads(updates: &Path) {
    let Ok(list) = std::fs::read_dir(updates.join("download")) else { return };
    for entry in list.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("setup-") && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// `--resume <file>` from the command line.
pub fn resume_arg(args: impl IntoIterator<Item = std::ffi::OsString>) -> Option<PathBuf> {
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        if a == "--resume" {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

#[cfg(test)]
#[path = "update_resume_tests.rs"]
mod tests;
