//! Cut-off media preparations. The engine's `prepare-editor` works in
//! `<cache>\editor\.<key>.<pid>-<n>\` (`recall/src/editor_media.rs` `create_work_dir`) and
//! renames that folder to `<key>` when done; only its own error path removes it. An engine
//! killed mid-way (another clip opened during "Preparing sound…", a time limit) never does, and
//! its partial stems held the cache budget for good. They go here: right after the kill, and at
//! the first budget pass of each cache root once their engine no longer runs. Strict: only a
//! real folder with exactly that name, directly in the editor folder, and only the engine's own
//! files in it (`unit_files::remove_engine_folder`).
use std::io;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

use crate::services::process_tree::ProcessState;

/// The engine's program file (`backend::engine_path`).
const ENGINE_EXE: &str = "recall.exe";

/// Who holds a partial folder's pid now. Production: [`OWNER`].
pub(super) type OwnerFn = fn(u32) -> ProcessState;
pub(super) const OWNER: OwnerFn = crate::services::process_tree::state;

/// Runs a `prepare-editor` engine hidden, killed at `limit` or once `stop` says so; an engine it has to kill (stop or
/// time limit) leaves no partial folder in `editor`.
pub fn run_prepare(command: &mut Command, editor: &Path, lowered: bool, limit: Duration, stop: &dyn Fn() -> bool) -> io::Result<Output> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // The killed engine's handle is still open in the callback, so its pid is not reused yet.
    super::ffmpeg::watch_then(super::io_priority::spawn(command, lowered)?, limit, stop, &|pid| {
        let freed = remove_partials_of(editor, pid);
        if freed > 0 {
            eprintln!("editor cache: removed a cut-off preparation of engine {pid} ({} MiB)", freed >> 20);
        }
    })
}

/// `.<16 hex>.<pid>-<n>`: the pid of an engine work folder, None for any other name.
pub(super) fn partial_pid(name: &str) -> Option<u32> {
    let (key, tail) = name.strip_prefix('.')?.split_once('.')?;
    let (pid, attempt) = tail.split_once('-')?;
    let digits = |part: &str| !part.is_empty() && part.len() <= 10 && part.bytes().all(|b| b.is_ascii_digit());
    if !super::owned::is_id(key) || !digits(pid) || !digits(attempt) {
        return None;
    }
    pid.parse().ok()
}

/// Deletes the partial folders of engine `pid` in `editor` (just killed). Returns bytes freed.
pub(super) fn remove_partials_of(editor: &Path, pid: u32) -> u64 {
    if super::guard::blocked(editor).is_some() {
        return 0;
    }
    let listing = std::fs::read_dir(editor).into_iter().flatten().flatten();
    listing.filter(|entry| partial_pid(&entry.file_name().to_string_lossy()) == Some(pid)).map(|entry| remove_partial(editor, &entry.path())).sum()
}

/// Deletes the partial folders in `<root>\editor` whose engine is gone: its pid runs nothing,
/// or now runs another program that started after the folder was made (a reused pid). A
/// running engine, a pid that can't be checked or a folder without a creation time keeps
/// the folder. Returns bytes freed.
pub(super) fn sweep_dead_partials(root: &Path, owner: OwnerFn) -> u64 {
    if super::guard::blocked(root).is_some() {
        return 0;
    }
    let editor = root.join("editor");
    let mut freed = 0;
    for entry in std::fs::read_dir(&editor).into_iter().flatten().flatten() {
        let Some(pid) = partial_pid(&entry.file_name().to_string_lossy()) else { continue };
        let Ok(created) = std::fs::symlink_metadata(entry.path()).and_then(|meta| meta.created()) else { continue };
        if owner_gone(owner(pid), created) {
            freed += remove_partial(&editor, &entry.path());
        }
    }
    freed
}

fn owner_gone(owner: ProcessState, created: SystemTime) -> bool {
    match owner {
        ProcessState::Gone => true,
        ProcessState::Running { started, image } => !is_engine(&image) && created < started,
        ProcessState::Unknown => false,
    }
}

fn is_engine(image: &str) -> bool {
    Path::new(image).file_name().is_some_and(|name| name.eq_ignore_ascii_case(ENGINE_EXE))
}

/// Deletes the engine files in `path` and then the folder once empty, only when `path` is a
/// real folder (no link, no junction) directly inside `editor`. Returns bytes freed.
fn remove_partial(editor: &Path, path: &Path) -> u64 {
    use std::os::windows::fs::MetadataExt;
    const REPARSE_POINT: u32 = 0x400;
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() || meta.file_type().is_symlink() || meta.file_attributes() & REPARSE_POINT != 0 {
        return 0;
    }
    let (Ok(base), Ok(real)) = (std::fs::canonicalize(editor), std::fs::canonicalize(path)) else { return 0 };
    let named = real.file_name().is_some_and(|name| partial_pid(&name.to_string_lossy()).is_some());
    if !named || real.parent() != Some(base.as_path()) {
        return 0;
    }
    super::unit_files::remove_engine_folder(&real)
}

#[cfg(test)]
#[path = "prepare_leftovers_tests.rs"]
mod tests;
