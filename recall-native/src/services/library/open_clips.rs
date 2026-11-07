//! The clips open in the app, published for the recorder's storage cap
//! (`recall-hdr/src/record_storage_keep.rs`), which never moves an open clip to the Recycle Bin:
//! the clip loaded in the editor/player ([`publish`]) and every clip an export or pre-encode is
//! reading ([`exporting`], `backend_export.rs` `engine_export`).
//!
//! `open-clips.json` beside `library.json`: `{"paths": [...], "owners": {"<pid>": [...]}}`. The
//! recorder reads `paths`. Each app process rewrites only its own `owners` entry and keeps those
//! of other app processes still running (a closed window's process may still be finishing an
//! export while a new window runs), under one named lock, so neither drops the other's clips.
//! The editor clip stays published until another opens; it is cleared at start and when the
//! window closes, export sources when their export ends. A dead process's entry goes at the
//! next write, so a crash only keeps its clips until then.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Named lock around the read-modify-write of the file (all app processes of this logon).
const LOCK: &str = r"Local\RecallOpenClips";

/// This process's open clips.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Own {
    editor: Option<String>,
    exports: Vec<String>,
}

impl Own {
    fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.editor.iter().chain(&self.exports).cloned().collect();
        paths.dedup();
        paths
    }
}

static OWN: Mutex<Own> = Mutex::new(Own { editor: None, exports: Vec::new() });

/// `<folder of library.json>\open-clips.json`.
pub(super) fn path_beside(library: &Path) -> PathBuf {
    library.with_file_name("open-clips.json")
}

/// Publishes the editor's clip (None = nothing open). A failure is logged: the cap then still
/// keeps a clip played in the last ten minutes (`library.json` `played`).
pub fn publish(open: Option<&str>) {
    change(&OWN, |own| own.editor = open.map(str::to_owned), write_own);
}

/// Publishes `path` as open while the returned guard lives (an export reading it).
pub fn exporting(path: &str) -> Exporting {
    change(&OWN, |own| own.exports.push(path.to_owned()), write_own);
    Exporting { path: path.to_owned(), state: &OWN, write: write_own }
}

/// An export source published as open; dropping it unpublishes it.
pub struct Exporting {
    path: String,
    state: &'static Mutex<Own>,
    write: fn(&[String]),
}

impl Drop for Exporting {
    fn drop(&mut self) {
        let path = std::mem::take(&mut self.path);
        change(self.state, |own| {
            if let Some(i) = own.exports.iter().position(|p| *p == path) {
                own.exports.remove(i);
            }
        }, self.write);
    }
}

/// Applies `edit` to this process's list, then writes it.
fn change(state: &Mutex<Own>, edit: impl FnOnce(&mut Own), write: fn(&[String])) {
    let mut own = state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    edit(&mut own);
    write(&own.paths());
}

fn write_own(paths: &[String]) {
    let Some(library) = super::store::meta_path(std::env::var_os("RECALL_LIBRARY_META"), std::env::var_os("APPDATA")) else { return };
    // Best effort: a lock that cannot be had in 2 s (a stuck process) still lets this write go.
    let _lock = crate::win_fileop::bin_lock::acquire(LOCK, Duration::from_secs(2));
    if let Err(error) = write_to(&path_beside(&library), std::process::id(), paths, pid_alive) {
        eprintln!("open clip not published: {error}");
    }
}

/// `pid` still runs (as `install_wait.rs`; a process this user may not query counts as running).
fn pid_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED};
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: query-only handle, closed below.
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => {
            let mut code = 0u32;
            // SAFETY: a valid handle and an out pointer to a local.
            let ok = unsafe { GetExitCodeProcess(handle, &mut code) }.is_ok();
            // SAFETY: the handle is ours.
            let _ = unsafe { CloseHandle(handle) };
            ok && code == STILL_ACTIVE
        }
        Err(e) => e.code() == ERROR_ACCESS_DENIED.to_hresult(),
    }
}

/// Writes `own` as process `pid`'s entry, keeping the entries of other processes `alive` says
/// still run: temporary file, flushed, renamed over the old one.
pub(super) fn write_to(file: &Path, pid: u32, own: &[String], alive: fn(u32) -> bool) -> Result<(), String> {
    use std::io::Write;
    let mut owners: BTreeMap<String, Vec<String>> = std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| serde_json::from_value(value.get("owners")?.clone()).ok())
        .unwrap_or_default();
    owners.retain(|owner, _| owner.parse::<u32>().is_ok_and(|other| other != pid && alive(other)));
    if !own.is_empty() {
        owners.insert(pid.to_string(), own.to_vec());
    }
    let mut paths: Vec<&String> = owners.values().flatten().collect();
    paths.sort();
    paths.dedup();
    let bytes = serde_json::to_vec(&serde_json::json!({ "paths": paths, "owners": owners })).map_err(|e| e.to_string())?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let temporary = file.with_extension(format!("json.{pid}.tmp"));
    let written = std::fs::File::create(&temporary).and_then(|mut f| f.write_all(&bytes).and_then(|()| f.sync_all()));
    if let Err(e) = written.and_then(|()| std::fs::rename(&temporary, file)) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("write {}: {e}", file.display()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "open_clips_tests.rs"]
mod tests;
