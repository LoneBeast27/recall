//! Edit-project write policy for SMR libraries: autosave lands on the SSD
//! (`projects/<id>.json`); the `.recall-edit.json` sidecar beside the clip is written only
//! on an explicit flush (leave / close / export) and only when its bytes would change.
use std::path::{Path, PathBuf};

/// Which saved project a prepared clip loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectSource {
    WorkingCopy,
    Sidecar,
}

/// `projects/<identity>.json` under `root`.
pub fn working_copy_path(root: &Path, file_name: &str, bytes: u64, mtime_secs: u64) -> PathBuf {
    root.join("projects").join(format!("{}.json", super::identity(file_name, bytes, mtime_secs)))
}

/// Load order by modification time (ms): the newer file first; on a tie the sidecar (a
/// flush writes the sidecar after the working copy it came from). The caller tries the
/// next entry when one fails validation.
pub fn project_order(working_ms: Option<u64>, sidecar_ms: Option<u64>) -> Vec<ProjectSource> {
    match (working_ms, sidecar_ms) {
        (Some(working), Some(sidecar)) if working > sidecar => vec![ProjectSource::WorkingCopy, ProjectSource::Sidecar],
        (Some(_), Some(_)) => vec![ProjectSource::Sidecar, ProjectSource::WorkingCopy],
        (Some(_), None) => vec![ProjectSource::WorkingCopy],
        (None, Some(_)) => vec![ProjectSource::Sidecar],
        (None, None) => Vec::new(),
    }
}

/// Whether a flush must write the sidecar. An existing sidecar is rewritten only when the
/// bytes differ. Without one, a sidecar is created only for real edits: an autosaved
/// working copy exists, or the plan differs from the untouched default.
pub fn should_flush(existing: Option<&[u8]>, next: &[u8], working_copy_exists: bool, plan_is_default: bool) -> bool {
    match existing {
        Some(current) => current != next,
        None => working_copy_exists || !plan_is_default,
    }
}

/// Write to a unique temp sibling, then rename over the target.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let temporary = super::temp_sibling(path);
    std::fs::write(&temporary, bytes).map_err(|error| format!("write {}: {error}", path.display()))?;
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("publish {}: {error}", path.display())
    })
}

/// Modification time in Unix ms, if the file exists.
pub(crate) fn modified_ms(path: &Path) -> Option<u64> {
    let modified = path.metadata().ok()?.modified().ok()?;
    modified.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
}
