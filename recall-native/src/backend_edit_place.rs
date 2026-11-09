//! Where a clip's edit file lives: beside the clip for Recall's own clips (the recordings folder
//! and the folders Recall manages), on C: (`imports::edit_file`) for a clip in an imported folder.
//!
//! The folder list is the UI's own (`share_folders`, kept in step by `Root` whenever its list
//! changes), the one the delete dialog uses, not a fresh read of folders.json per save: a read
//! that failed would make every imported clip look like Recall's own and put its edits beside
//! it. With no shared list (tests, tools) folders.json is read; one that could not be read
//! refuses the write.
//!
//! A clip of a folder Recall no longer manages may still have its edits beside it from before:
//! they are read (never written or removed) until the first flush writes the C: copy.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::library_dir_from;
use crate::services::{library::{folders::Folders, imports}, storage};

static SHARED: Mutex<Option<Folders>> = Mutex::new(None);

/// The UI's folder list changed (start-up load, Settings, a followed recordings folder).
pub fn share_folders(folders: &Folders) {
    *SHARED.lock().unwrap_or_else(|p| p.into_inner()) = Some(folders.clone());
}

pub(crate) fn folders_now() -> Folders {
    SHARED.lock().unwrap_or_else(|p| p.into_inner()).clone().unwrap_or_else(Folders::load)
}

/// The edit file a save of `source` writes.
pub(super) fn sidecar(source: &str) -> Result<PathBuf, String> {
    sidecar_in(source, &folders_now())
}

pub(super) fn sidecar_in(source: &str, folders: &Folders) -> Result<PathBuf, String> {
    if let Some(why) = &folders.write_block {
        return Err(format!("{why}; edits are not saved until Recall restarts"));
    }
    let clip = Path::new(source);
    let recordings = library_dir_from(folders);
    if imports::is_owned(clip, &recordings, &folders.former_recordings)
        && clip.parent().is_some_and(|parent| !overlaps_link(parent, folders))
    {
        return beside(source);
    }
    let sidecar = imports::edit_file(clip).ok_or("APPDATA is not set")?;
    check_write_dir(sidecar.parent().ok_or("No edit folder")?, clip, folders, false)?;
    Ok(sidecar)
}

/// Planned output path for the editor and the export worker. An external clip's export
/// belongs to Recall's recordings export folder, including when the source was unlinked.
pub(crate) fn export_dir_for(clip: &Path, folders: &Folders) -> Option<PathBuf> {
    let recordings = library_dir_from(folders);
    if imports::is_owned(clip, &recordings, &folders.former_recordings) {
        storage::export_dir(clip, &recordings)
    } else {
        storage::export_dir(&recordings.join("clip.mp4"), &recordings)
    }
}

/// Validate the physical destination before creating an output directory or starting a sweep.
pub(crate) fn checked_export_dir(clip: &Path, folders: &Folders) -> Result<PathBuf, String> {
    if let Some(why) = &folders.write_block {
        return Err(format!("{why}. Recall needs its folder list before it can write an export."));
    }
    let directory = export_dir_for(clip, folders).ok_or("The export has no destination folder")?;
    let owned = imports::is_owned(clip, &library_dir_from(folders), &folders.former_recordings);
    check_write_dir(&directory, clip, folders, owned)?;
    Ok(directory)
}

pub(crate) fn overlaps_link(directory: &Path, folders: &Folders) -> bool {
    folders.linked().any(|link| storage::guard::folders_overlap(directory, Path::new(link)))
}

fn check_write_dir(directory: &Path, clip: &Path, folders: &Folders, owned: bool) -> Result<(), String> {
    if overlaps_link(directory, folders) || (!owned && clip.parent().is_some_and(|source| storage::guard::folders_overlap(directory, source))) {
        return Err("Recall needs a write folder outside the linked recordings. Choose a separate recordings folder in Settings → Media.".into());
    }
    Ok(())
}

fn beside(source: &str) -> Result<PathBuf, String> {
    let p = Path::new(source);
    let name = p.file_name().ok_or("source has no filename")?.to_string_lossy();
    Ok(p.with_file_name(format!("{name}.recall-edit.json")))
}

/// Read-only: the beside-clip edit file of an imported clip whose own edit file (`sidecar`, on
/// C:) is not there yet, when one exists (left from when its folder was Recall's).
pub(super) fn beside_fallback(source: &str, sidecar: &Path) -> Option<PathBuf> {
    let old = beside(source).ok()?;
    (old != sidecar && !sidecar.exists() && old.exists()).then_some(old)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit_model::{ClipInfo, EditPlan, ExportSettings, TimeRange};

    #[test]
    fn linked_clips_keep_their_original_folder_read_only() {
        let root = crate::test_temp::TempDir::sandbox("folder-mapper");
        let recordings = root.join("Recall");
        let source = root.join("ShadowPlay");
        std::fs::create_dir_all(&recordings).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        let clip = source.join("Game.mp4");
        std::fs::write(&clip, b"original").unwrap();
        let mut folders = Folders {
            recordings: Some(recordings.to_string_lossy().into_owned()),
            imported: vec![source.to_string_lossy().into_owned()],
            ..Folders::default()
        };

        let edit = sidecar_in(&clip.to_string_lossy(), &folders).unwrap();
        assert_eq!(Some(edit), imports::edit_file(&clip));
        let exports = checked_export_dir(&clip, &folders).unwrap();
        assert_eq!(exports, recordings.join("exports"));
        assert!(!source.join("exports").exists());

        let blocked = Folders {
            recordings: Some(source.join("Recall").to_string_lossy().into_owned()),
            ..folders.clone()
        };
        assert!(checked_export_dir(&clip, &blocked).is_err());

        folders.forget(&source.to_string_lossy());
        assert_eq!(checked_export_dir(&clip, &folders).unwrap(), exports, "a stale clip remains external");
        let removed = crate::services::library::delete_or_hide(std::slice::from_ref(&clip), &folders);
        assert_eq!((removed.hidden, removed.recycled), (1, 0));
        assert_eq!(std::fs::read(&clip).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(&source).unwrap().count(), 1, "the source folder gained no files");
    }

    #[test]
    fn the_uis_folder_list_places_edits_and_a_released_folders_edits_still_open() {
        let dir = std::env::temp_dir().join(format!("recall-edit-place-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("released.mp4").to_string_lossy().into_owned();
        std::fs::write(&clip, b"clip").unwrap();
        // The UI's list says the folder is imported (folders.json on disk does not list it).
        share_folders(&Folders { imported: vec![dir.to_string_lossy().into_owned()], ..Folders::default() });
        let own = sidecar(&clip).unwrap();
        assert_eq!(Some(own.clone()), imports::edit_file(Path::new(&clip)), "edits of an imported clip go to C:, by the UI's list");
        // Edits beside the clip from when its folder was Recall's: they open, and stay as they are.
        let info = ClipInfo { path: clip.clone(), duration: 10.0, width: 1, height: 1, video_fps: 60.0, source_bytes: 4, source_modified_ms: 5678, audio: Vec::new(), keyframes: Vec::new() };
        let project = super::super::Project { version: 1, source_path: clip.clone(), source_bytes: 4, source_modified_ms: 5678, plan: EditPlan { mix_down: None, ranges: vec![TimeRange::new(1.0, 9.0)], audio: Vec::new() }, export_fps: 60, export: ExportSettings::default() };
        let old = beside(&clip).unwrap();
        let bytes = serde_json::to_vec(&project).unwrap();
        std::fs::write(&old, &bytes).unwrap();
        let (loaded, _, ours, _) = super::super::load_project(&info);
        assert_eq!(loaded.map(|p| p.plan.ranges), Some(vec![TimeRange::new(1.0, 9.0)]), "the beside-clip edits are read");
        assert!(ours, "the first flush may write the C: copy");
        assert_eq!(std::fs::read(&old).unwrap(), bytes, "the beside-clip file is left as it was");
        assert!(!own.exists(), "opening writes nothing");
        // A list that could not be read refuses rather than guessing.
        let blocked = Folders { write_block: Some("folders.json could not be read".into()), ..Folders::default() };
        assert!(sidecar_in(&clip, &blocked).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
