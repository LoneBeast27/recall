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
    // Owned wins over linked: a clip Recall recorded keeps its edits beside it, also under a
    // linked parent folder (Videos linked, clips in Videos\Recall).
    if imports::is_owned(clip, &recordings, &folders.former_recordings, &folders.imported) {
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
    if imports::is_owned(clip, &recordings, &folders.former_recordings, &folders.imported) {
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
    let owned = imports::is_owned(clip, &library_dir_from(folders), &folders.former_recordings, &folders.imported);
    check_write_dir(&directory, clip, folders, owned)?;
    Ok(directory)
}

/// Whether writing files into `directory` would write into a linked folder (read-only to
/// Recall): it lies in a linked tree outside every owned folder below that link, or it holds a
/// link without lying in an owned folder. Owned folders are the recordings folder and the former
/// ones; one under a linked parent stays writable, `<recordings>\exports` included.
pub(crate) fn link_blocks_write(directory: &Path, folders: &Folders) -> bool {
    use storage::guard::folder_within as within;
    let recordings = library_dir_from(folders);
    let owned: Vec<&Path> = std::iter::once(recordings.as_path()).chain(folders.former_recordings.iter().map(Path::new)).collect();
    let in_owned = |below: Option<&Path>| owned.iter().any(|own| within(directory, own) && below.is_none_or(|link| within(own, link)));
    folders.linked().map(Path::new).any(|link| {
        (within(directory, link) && !in_owned(Some(link))) || (within(link, directory) && !in_owned(None))
    })
}

fn check_write_dir(directory: &Path, clip: &Path, folders: &Folders, owned: bool) -> Result<(), String> {
    if link_blocks_write(directory, folders) || (!owned && clip.parent().is_some_and(|source| storage::guard::folders_overlap(directory, source))) {
        return Err("Recall needs a write folder outside the linked recordings. Choose a separate recordings folder in Settings → Storage.".into());
    }
    Ok(())
}

fn beside(source: &str) -> Result<PathBuf, String> {
    let p = Path::new(source);
    let name = p.file_name().ok_or("source has no filename")?.to_string_lossy();
    Ok(p.with_file_name(format!("{name}.recall-edit.json")))
}

/// Read-only: the beside-clip edit file of a clip whose edits are written to C: (`sidecar`),
/// left from when its folder was Recall's or before the folder overlapped a link, when it is
/// there and the C: one is missing or older (by modified time). It is never written or removed.
pub(super) fn beside_fallback(source: &str, sidecar: &Path) -> Option<PathBuf> {
    let old = beside(source).ok()?;
    if old == sidecar {
        return None;
    }
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let beside_time = modified(&old)?;
    match modified(sidecar) {
        Some(own) if own >= beside_time => None,
        Some(_) => Some(old),
        None => (!sidecar.exists()).then_some(old),
    }
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

    /// The editor saves by `info.path`, the canonical `\\?\` form: an owned clip's edits still go
    /// beside it, not to APPDATA (the viewer check's lib-undone run, 2026-11-10).
    #[test]
    fn an_owned_clip_opened_by_its_verbatim_path_keeps_its_edits_beside_it() {
        let root = crate::test_temp::TempDir::sandbox("edit-beside-verbatim");
        std::fs::create_dir_all(root.join("lib-undone")).unwrap();
        // The folder list holds the plain form (the sandbox path has a `..` in it).
        let recordings = storage::plain_path(&std::fs::canonicalize(root.join("lib-undone")).unwrap());
        let clip = recordings.join("undone.mp4");
        std::fs::write(&clip, b"clip").unwrap();
        let source = std::fs::canonicalize(&clip).unwrap().to_string_lossy().into_owned();
        assert!(source.starts_with(r"\\?\"), "{source}");
        let folders = Folders { recordings: Some(recordings.to_string_lossy().into_owned()), ..Folders::default() };
        let edit = sidecar_in(&source, &folders).unwrap();
        assert_eq!(storage::plain_path(&edit), recordings.join("undone.mp4.recall-edit.json"));
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

    #[test]
    fn a_newer_edit_file_beside_a_linked_clip_loads_and_stays_byte_unchanged() {
        let root = crate::test_temp::TempDir::sandbox("edit-beside-newer");
        let linked = root.join("Videos");
        std::fs::create_dir_all(&linked).unwrap();
        let clip = linked.join("old.mp4");
        std::fs::write(&clip, b"clip").unwrap();
        let clip = clip.to_string_lossy().into_owned();
        let folders = Folders { recordings: Some(root.join("Recall").to_string_lossy().into_owned()), imported: vec![linked.to_string_lossy().into_owned()], ..Folders::default() };
        let own = sidecar_in(&clip, &folders).unwrap();
        assert_eq!(Some(own.clone()), imports::edit_file(Path::new(&clip)), "new writes stay in APPDATA");
        let info = ClipInfo { path: clip.clone(), duration: 10.0, width: 1, height: 1, video_fps: 60.0, source_bytes: 4, source_modified_ms: 5678, audio: Vec::new(), keyframes: Vec::new() };
        let project = |a: f64| super::super::Project { version: 1, source_path: clip.clone(), source_bytes: 4, source_modified_ms: 5678, plan: EditPlan { mix_down: None, ranges: vec![TimeRange::new(a, 9.0)], audio: Vec::new() }, export_fps: 60, export: ExportSettings::default() };
        // An older APPDATA copy (an hour ago), and the newer edits beside the clip.
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        std::fs::write(&own, serde_json::to_vec(&project(2.0)).unwrap()).unwrap();
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&own).unwrap().set_modified(hour_ago).unwrap();
        let old = beside(&clip).unwrap();
        let bytes = serde_json::to_vec(&project(1.0)).unwrap();
        std::fs::write(&old, &bytes).unwrap();
        let (loaded, _, ours, _) = super::super::load_project(&info);
        assert_eq!(loaded.map(|p| p.plan.ranges), Some(vec![TimeRange::new(1.0, 9.0)]), "the newer beside-clip edits load");
        assert!(ours);
        // A save goes to APPDATA; the beside file is left byte for byte.
        let saved = serde_json::to_vec(&project(3.0)).unwrap();
        super::super::sidecar_flush::sidecar_task(&own, Path::new(&clip), &saved, false, ours).unwrap();
        assert_eq!(std::fs::read(&own).unwrap(), saved);
        assert_eq!(std::fs::read(&old).unwrap(), bytes, "the beside-clip file is unchanged");
        let _ = std::fs::remove_file(&own);
    }
}
