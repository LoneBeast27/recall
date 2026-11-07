//! The clip open in the editor/player, published for the recorder's storage cap
//! (`recall-hdr/src/record_storage_keep.rs`), which never moves an open clip to the Recycle Bin.
//! `open-clips.json` beside `library.json`: `{"paths": ["E:\\Clips\\x.mp4"]}`. Written when a
//! clip opens (it stays the open clip until another opens), emptied at start and when the
//! window closes, so a crash leaves at most one stale entry, which only keeps that clip.
use std::path::{Path, PathBuf};

/// `<folder of library.json>\open-clips.json`.
pub(super) fn path_beside(library: &Path) -> PathBuf {
    library.with_file_name("open-clips.json")
}

/// Publishes `open` (None = nothing open). A failure is logged: the cap then still keeps a
/// clip played in the last ten minutes (`library.json` `played`).
pub fn publish(open: Option<&str>) {
    let Some(library) = super::store::meta_path(std::env::var_os("RECALL_LIBRARY_META"), std::env::var_os("APPDATA")) else { return };
    if let Err(error) = write_to(&path_beside(&library), open) {
        eprintln!("open clip not published: {error}");
    }
}

/// Writes the list durably: temporary file, flushed, renamed over the old one.
pub(super) fn write_to(file: &Path, open: Option<&str>) -> Result<(), String> {
    use std::io::Write;
    let bytes = serde_json::to_vec(&serde_json::json!({ "paths": open.into_iter().collect::<Vec<_>>() })).map_err(|e| e.to_string())?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let temporary = file.with_extension("json.tmp");
    let written = std::fs::File::create(&temporary).and_then(|mut f| f.write_all(&bytes).and_then(|()| f.sync_all()));
    if let Err(e) = written.and_then(|()| std::fs::rename(&temporary, file)) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("write {}: {e}", file.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_clip_is_published_beside_the_library_and_cleared() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("recall-open-{}-{nonce}", std::process::id()));
        let file = path_beside(&dir.join("library.json"));
        assert_eq!(file, dir.join("open-clips.json"));
        write_to(&file, Some(r"E:\Clips\G__a.mp4")).unwrap();
        let read = |f: &Path| serde_json::from_slice::<serde_json::Value>(&std::fs::read(f).unwrap()).unwrap();
        assert_eq!(read(&file), serde_json::json!({ "paths": [r"E:\Clips\G__a.mp4"] }), "the shape the recorder reads");
        write_to(&file, None).unwrap();
        assert_eq!(read(&file), serde_json::json!({ "paths": [] }));
        assert!(!file.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
