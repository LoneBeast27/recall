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

/// What a flush does to the sidecar beside the clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarStep {
    Keep,
    Write,
    Remove,
}

/// A sidecar exists only while the plan holds real edits: it is what marks a clip Edited and
/// keeps it from the storage limit. Undoing every edit removes it. Identical bytes are not
/// written again. A sidecar this session does not own (`ours` false: it could not be read or
/// loaded at open) may hold edits the editor never saw: it stays as it is.
pub fn sidecar_step(existing: Option<&[u8]>, next: &[u8], no_edits: bool, ours: bool) -> SidecarStep {
    match (existing, no_edits) {
        _ if !ours => SidecarStep::Keep,
        (_, true) => SidecarStep::Remove,
        (Some(current), false) if current == next => SidecarStep::Keep,
        (_, false) => SidecarStep::Write,
    }
}

/// Where exports of `clip` land. With the Recall layout (recordings in `...\Recall\Raw`) every
/// export goes to the `Edited` folder beside `Raw`; otherwise to `exports` beside the clip.
pub fn export_dir(clip: &Path, recordings: &Path) -> Option<std::path::PathBuf> {
    let raw = recordings.file_name().is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("raw"));
    match recordings.parent() {
        Some(recall) if raw => Some(recall.join("Edited")),
        _ => clip.parent().map(|folder| folder.join("exports")),
    }
}

/// `clip` has a finished export, in its export folder ([`export_dir`]) or in the older
/// `exports` folder beside it. Exported clips stay protected from the storage limit, so their
/// sidecar stays when the edits are undone.
pub fn has_export(clip: &Path, recordings: &Path) -> bool {
    let Some(stem) = clip.file_stem().and_then(|s| s.to_str()) else { return false };
    let names = [format!("{stem}-edited-").to_lowercase(), format!("{stem}-trim-").to_lowercase()];
    let mut folders: Vec<std::path::PathBuf> = export_dir(clip, recordings).into_iter().collect();
    if let Some(beside) = clip.parent().map(|folder| folder.join("exports")).filter(|beside| !folders.contains(beside)) {
        folders.push(beside);
    }
    folders.iter().flat_map(|folder| std::fs::read_dir(folder).into_iter().flatten().flatten()).any(|item| {
        let name = item.file_name().to_string_lossy().to_lowercase();
        names.iter().any(|prefix| name.starts_with(prefix))
    })
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
