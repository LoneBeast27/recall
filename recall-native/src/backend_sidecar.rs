//! The sidecar step of a flush (`flush_project`), run on the clip's disk lane.
use super::*;

/// Writes, keeps or removes `path` (the sidecar of `clip`) for `bytes`. `default`: the plan has
/// no edits. `ours` false leaves an existing sidecar untouched (see [`load_project`]).
pub(super) fn sidecar_task(path:&Path,clip:&Path,bytes:&[u8],default:bool,ours:bool)->Result<Flushed,String>{
    let existing=std::fs::read(path).ok();let name=path.to_string_lossy().into_owned();
    // Deleted while this flush waited (Delete runs on the io thread): no orphan beside nothing.
    if !clip.exists(){return Ok(Flushed::Nothing);}
    let ours=ours&&(existing.is_some()||!path.exists());
    match storage::sidecar_step(existing.as_deref(),bytes,default&&!storage::has_export(clip,&library_dir()),ours){
        storage::SidecarStep::Keep=>Ok(Flushed::Current(name)),
        storage::SidecarStep::Write=>{storage::write_atomic(path,bytes)?;Ok(Flushed::Current(name))}
        storage::SidecarStep::Remove=>{
            if existing.is_none(){return Ok(Flushed::Nothing);}
            std::fs::remove_file(path).map_err(|e|format!("remove {name}: {e}"))?;Ok(Flushed::Removed(name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_sidecar_appears_beside_a_clip_deleted_before_the_flush_ran() {
        // R2-7: leave the editor, delete the clip at once: Delete (io thread) recycled the clip
        // while the flush (edits thread) waited, then wrote a new orphan sidecar beside nothing.
        let dir = std::env::temp_dir().join(format!("recall-orphan-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("gone.mp4");
        let path = dir.join("gone.mp4.recall-edit.json");
        let _ = std::fs::remove_file(&path);
        assert_eq!(sidecar_task(&path, &clip, b"{}", false, true), Ok(Flushed::Nothing));
        assert!(!path.exists(), "an orphan sidecar was written");
        std::fs::write(&clip, b"clip").unwrap();
        assert!(matches!(sidecar_task(&path, &clip, b"{}", false, true), Ok(Flushed::Current(_))));
        assert!(path.exists(), "a clip that is there still gets its sidecar");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
