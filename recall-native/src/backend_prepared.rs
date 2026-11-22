//! An engine cache entry read in the app. `recall.exe --prepare-editor` on an entry it already
//! has only reads `clip.json` and the clip's native-height tag, but starting the engine for
//! that costs ~40 ms, and for an entry keyed on the original it opens the clip on its hard
//! drive (35-300 ms cold). The checks match `recall/src/editor_media.rs` (`prepare_clip_cached`,
//! `valid_cache`): anything they would not accept goes to the engine as before.
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::edit_model::{ClipInfo, NATIVE_HEIGHT_TAG, native_height_from};
use crate::services::library::mp4_tags;
use crate::services::storage;

use super::{cache_dir, engine_command, modified_ms};

/// Waveform points per track in an engine entry (`editor_media.rs` `WAVE_BINS`).
const WAVE_BINS: usize = 1024;

/// The entry for `input` (canonical, as the engine keys it), when the engine would take it as
/// it is. `tags_from`: a byte-identical file to read the native-height tag from (the SSD copy).
pub(super) fn cached_entry(editor_root: &Path, input: &Path, tags_from: &Path) -> Option<ClipInfo> {
    let metadata = input.metadata().ok().filter(|m| m.is_file())?;
    let (bytes, modified) = (metadata.len(), modified_ms(&metadata));
    let manifest = storage::engine_cache_dir(editor_root, input, bytes, modified).join("clip.json");
    let mut info: ClipInfo = serde_json::from_slice(&std::fs::read(manifest).ok()?).ok()?;
    if !accepted(&info, input, bytes, modified) {
        return None;
    }
    // The engine reads the tag afresh at every prepare (older entries lack it).
    let recorded = info.height;
    info.native_height = native_height_from(mp4_tags::file_tag(tags_from, NATIVE_HEIGHT_TAG).as_deref()).filter(|native| *native > recorded);
    Some(info)
}

/// What `prepare_clip_cached` returns as it is. Entries without keyframes are refreshed by the
/// engine, so they are not taken here.
fn accepted(info: &ClipInfo, input: &Path, bytes: u64, modified: u64) -> bool {
    info.source_bytes == bytes
        && info.source_modified_ms == modified
        && info.path == input.to_string_lossy()
        && info.duration.is_finite()
        && info.duration >= 0.0
        && !info.keyframes.is_empty()
        && info.audio.iter().all(|track| track.waveform.len() == WAVE_BINS && Path::new(&track.preview_path).is_file())
}

// ---- The engine run, when the entry above is missing or stale ----
pub(crate) fn has_prepared(source:&Path,metadata:&std::fs::Metadata)->bool{
    storage::engine_cache_dir(&storage::cache_root().join("editor"),source,metadata.len(),modified_ms(metadata)).join("clip.json").is_file()
}
/// Preparing reads the whole clip once per track; past this it is not coming back.
fn prepare_limit(bytes:u64)->Duration{Duration::from_secs(120+bytes/(2_000_000))}
/// Engine input for a clip: whichever of (SSD copy, original) already has an engine cache
/// entry (no re-decode), else the copy (fast reads), else the original.
pub(super) fn engine_input(source:&Path,metadata:&std::fs::Metadata,copy:Option<&Path>)->PathBuf{
    let editor=storage::cache_root().join("editor");let (bytes,ms)=(metadata.len(),modified_ms(metadata));
    let has=|input:&Path|storage::engine_cache_dir(&editor,input,bytes,ms).join("clip.json").is_file();
    match copy.and_then(|c|std::fs::canonicalize(c).ok()){
        Some(copy) if has(&copy)=>copy,
        _ if has(source)=>source.to_path_buf(),
        Some(copy)=>copy,
        None=>source.to_path_buf(),
    }
}
/// `recall.exe --prepare-editor`: audio stems + waveforms into `<cache>\editor` (a cache hit reads
/// only `clip.json`). `lowered` starts the engine at very low I/O priority (HDD input).
/// Opening a clip the smart cache is preparing at the same moment races two engines to publish
/// one cache entry; the loser fails ("publish cache") although the entry is now complete, so it
/// asks once more and reads the winner's entry.
pub(super) fn run_engine_prepare(input:&Path,lowered:bool,stop:&dyn Fn()->bool)->Result<ClipInfo,String>{
    match run_engine_prepare_once(input,lowered,stop){
        Err(error) if lost_publish_race(&error)=>run_engine_prepare_once(input,lowered,stop),
        result=>result,
    }
}
pub(super) fn lost_publish_race(error:&str)->bool{error.contains("publish cache:")}
fn run_engine_prepare_once(input:&Path,lowered:bool,stop:&dyn Fn()->bool)->Result<ClipInfo,String>{
    let editor=cache_dir()?;let mut command=engine_command("prepare-editor")?;command.arg(input).arg(&editor);
    let limit=prepare_limit(input.metadata().map_or(0,|m|m.len()));
    // A cut-off engine (another clip opened, the limit) leaves no partial stems behind.
    let output=storage::run_prepare(&mut command,&editor,lowered,limit,stop).map_err(|e|format!("prepare media: {e}"))?;
    if !output.status.success(){return Err(format!("prepare media: {}",String::from_utf8_lossy(&output.stderr).trim()));}
    serde_json::from_slice(&output.stdout).map_err(|e|format!("read prepared media: {e}"))
}
#[cfg(test)]
#[path = "backend_prepared_tests.rs"]
mod tests;
