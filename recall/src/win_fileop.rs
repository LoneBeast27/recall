//! Send files to the Recycle Bin through `IFileOperation`, with a progress
//! sink that refuses any delete Windows would not recycle. Shared by the
//! storage limit (`recall-hdr`) and the library's delete (`recall-native`),
//! which include this file by path; `win_recycle.rs`'s precheck still runs
//! first.
//!
//! Measured on this Windows build (2026-11-07, sandbox files, probe in
//! `win_fileop_probe.rs` run on a hidden desktop):
//! - a `subst` drive and the `\localhost\L$` admin share (no Recycle Bin):
//!   `PreDeleteItem` came first with `dwFlags` 0x202 (no recycle bit); the sink
//!   refused and the file stayed;
//! - a file bigger than the bin (a sparse file): with `FOF_WANTNUKEWARNING`
//!   Windows showed its "Delete File" dialog BEFORE asking the sink (the call
//!   timed out, the file stayed); without it, Windows passed 0x282 (recycle
//!   bit set) and then deleted the file for good. So for size the sink is no
//!   guard: `win_recycle.rs`'s size precheck is, and the nuke warning backstop
//!   means Windows asks rather than deletes silently.
//!
//! Three layers, so no single one has to be perfect:
//! 1. The sink (primary). `PreDeleteItem` gets `TSF_DELETE_RECYCLE_IF_POSSIBLE`
//!    (0x80) in `dwFlags` when the item will go to the Recycle Bin, and
//!    returns `E_ABORT` whenever it is missing ([`allows_delete`]). It is
//!    attached twice: per item (`DeleteItem(item, sink)`) and to the whole
//!    operation (`Advise`), so an item is checked if Windows calls either.
//! 2. `FOF_WANTNUKEWARNING` (backstop). Should an item ever reach a permanent
//!    delete without the sink refusing it, Windows asks instead of deleting
//!    silently; `FOF_NOCONFIRMATION` answers every other confirmation.
//! 3. The count check (detection). Every distinct item the sinks saw is
//!    counted against the items queued; fewer means an item went through
//!    unchecked, and the call is reported as refused (the caller stops and
//!    tells the user). This runs after `PerformOperations`: it cannot undo,
//!    which is why layers 1 and 2 come first.
//!
//! Threading: `IFileOperation` is documented for single-threaded apartments
//! only, so every call runs on its own thread that initialises COM as an STA
//! (`COINIT_APARTMENTTHREADED`). The caller waits at most [`WAIT`]; after that
//! the call counts as refused and the sink refuses every later delete, so a
//! thread still running cannot delete anything afterwards.

use std::collections::HashSet;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::core::{implement, Ref, HRESULT, PCWSTR};
use windows::Win32::Foundation::E_ABORT;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem, SHCreateItemFromParsingName,
    FILEOPERATION_FLAGS, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, SIGDN_FILESYSPATH,
};

/// `TSF_DELETE_RECYCLE_IF_POSSIBLE`: the item will go to the Recycle Bin.
pub const TSF_DELETE_RECYCLE_IF_POSSIBLE: u32 = 0x80;

/// The flags every Recall `IFileOperation` delete uses (see the module doc).
pub const OP_FLAGS: u32 =
    FOFX_RECYCLEONDELETE.0 | FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0 | FOF_WANTNUKEWARNING.0;

/// How long a caller waits for one call before it counts as refused.
pub const WAIT: Duration = Duration::from_secs(30);

/// The sink's decision: a delete goes ahead only when Windows says it
/// recycles, and never after the caller stopped waiting.
pub fn allows_delete(dwflags: u32, abandoned: bool) -> bool {
    !abandoned && dwflags & TSF_DELETE_RECYCLE_IF_POSSIBLE != 0
}

/// How one recycle call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Recycled,
    /// Nothing was deleted for good: the sink refused an item Windows would
    /// have deleted permanently, the call was cancelled or timed out, or an
    /// item went through unchecked (reported, so the caller stops).
    Refused(String),
    Failed(String),
}

/// The outcome plus what the sinks saw (for the record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub outcome: Outcome,
    /// `dwFlags` of every `PreDeleteItem` call (both sinks).
    pub flags_seen: Vec<u32>,
    /// `PreDeleteItem` calls on the per-item sink and on the advised sink.
    pub calls: (usize, usize),
    /// Distinct items the sinks saw, and items queued.
    pub items: (usize, usize),
}

#[derive(Default)]
struct Seen {
    flags: Vec<u32>,
    calls: (usize, usize),
    paths: HashSet<String>,
    refused: bool,
}

#[implement(IFileOperationProgressSink)]
struct RecycleGuard {
    seen: Arc<Mutex<Seen>>,
    abandoned: Arc<AtomicBool>,
    /// true: attached with `Advise`; false: per item.
    advised: bool,
}

fn item_path(item: &Ref<IShellItem>) -> Option<String> {
    let item = item.ok().ok()?;
    // SAFETY: the shell allocates the string; it is freed here.
    unsafe {
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let text = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        text.map(|t| t.to_lowercase())
    }
}

impl IFileOperationProgressSink_Impl for RecycleGuard_Impl {
    fn PreDeleteItem(&self, dwflags: u32, psiitem: Ref<IShellItem>) -> windows::core::Result<()> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.flags.push(dwflags);
        if self.advised { seen.calls.1 += 1 } else { seen.calls.0 += 1 }
        if let Some(path) = item_path(&psiitem) {
            seen.paths.insert(path);
        }
        if allows_delete(dwflags, self.abandoned.load(Ordering::SeqCst)) {
            return Ok(());
        }
        seen.refused = true;
        Err(E_ABORT.into())
    }
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR, _: HRESULT, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreMoveItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostMoveItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR, _: HRESULT, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreCopyItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostCopyItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR, _: HRESULT, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostDeleteItem(&self, _: u32, _: Ref<IShellItem>, _: HRESULT, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR, _: &PCWSTR, _: u32, _: HRESULT, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn UpdateProgress(&self, _: u32, _: u32) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

/// `ERROR_CANCELLED` as an HRESULT, and the copy engine's "user cancelled".
const CANCELLED: [u32; 2] = [0x8007_04C7, 0x8027_0000];

/// The outcome from what the sinks saw and what the operation returned.
/// `items`: (distinct items the sinks saw, items queued).
pub fn classify(refused: bool, items: (usize, usize), perform: Result<(), u32>, aborted: bool) -> Outcome {
    if refused {
        return Outcome::Refused("Windows would have deleted it for good instead of moving it to the Recycle Bin".into());
    }
    let unchecked = items.0 < items.1;
    match perform {
        Err(code) if code == E_ABORT.0 as u32 || CANCELLED.contains(&code) => Outcome::Refused(format!("the Recycle Bin move was cancelled ({code:#x})")),
        _ if unchecked => Outcome::Refused(format!("{} of {} items were not checked before the delete", items.1 - items.0, items.1)),
        Err(code) => Outcome::Failed(format!("Recycle Bin move failed ({code:#x})")),
        Ok(()) if aborted => Outcome::Refused("the Recycle Bin move was cancelled".into()),
        Ok(()) => Outcome::Recycled,
    }
}

fn wide(path: &std::path::Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// One operation on the current (STA) thread.
fn perform(files: &[PathBuf], abandoned: Arc<AtomicBool>, flags: u32) -> Report {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let result = (|| -> windows::core::Result<(Result<(), u32>, bool)> {
        // SAFETY: COM is initialised as an STA on this thread by the caller.
        let op: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL)? };
        unsafe { op.SetOperationFlags(FILEOPERATION_FLAGS(flags))? };
        let per_item: IFileOperationProgressSink = RecycleGuard { seen: seen.clone(), abandoned: abandoned.clone(), advised: false }.into();
        let whole: IFileOperationProgressSink = RecycleGuard { seen: seen.clone(), abandoned: abandoned.clone(), advised: true }.into();
        let cookie = unsafe { op.Advise(&whole)? };
        for file in files {
            let path = wide(file);
            // SAFETY: a NUL-terminated path alive for the call.
            let item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None)? };
            unsafe { op.DeleteItem(&item, &per_item)? };
        }
        let performed = unsafe { op.PerformOperations() }.map_err(|e| e.code().0 as u32);
        let aborted = unsafe { op.GetAnyOperationsAborted() }.map(|b| b.as_bool()).unwrap_or(false);
        let _ = unsafe { op.Unadvise(cookie) };
        Ok((performed, aborted))
    })();
    let seen = seen.lock().unwrap_or_else(|p| p.into_inner());
    let items = (seen.paths.len(), files.len());
    let outcome = match result {
        Ok((performed, aborted)) => classify(seen.refused, items, performed, aborted),
        Err(e) if seen.refused => classify(true, items, Err(e.code().0 as u32), false),
        Err(e) => Outcome::Failed(format!("Recycle Bin move could not start: {e}")),
    };
    Report { outcome, flags_seen: seen.flags.clone(), calls: seen.calls, items }
}

/// Recycle `files` in one operation on a fresh STA thread, waiting at most `wait`.
pub fn recycle_files_within(files: &[PathBuf], wait: Duration) -> Report {
    recycle_with_flags(files, wait, OP_FLAGS)
}

/// [`recycle_files_within`] with another flag set (the live probe compares; production uses [`OP_FLAGS`]).
fn recycle_with_flags(files: &[PathBuf], wait: Duration, flags: u32) -> Report {
    let empty = |outcome| Report { outcome, flags_seen: Vec::new(), calls: (0, 0), items: (0, files.len()) };
    if files.is_empty() {
        return empty(Outcome::Recycled);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let abandoned = Arc::new(AtomicBool::new(false));
    let (list, flag) = (files.to_vec(), abandoned.clone());
    let spawned = std::thread::Builder::new().name("recall-recycle".into()).spawn(move || {
        // SAFETY: this thread owns its apartment; it is uninitialised below.
        let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        let report = if init.is_err() {
            Report { outcome: Outcome::Failed(format!("COM did not start ({:#x})", init.0 as u32)), flags_seen: Vec::new(), calls: (0, 0), items: (0, list.len()) }
        } else {
            let report = perform(&list, flag, flags);
            // SAFETY: matches the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
            report
        };
        let _ = tx.send(report);
    });
    if spawned.is_err() {
        return empty(Outcome::Failed("Recycle Bin worker thread failed to start".into()));
    }
    match rx.recv_timeout(wait) {
        Ok(report) => report,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // From here the sink refuses every delete, so the thread cannot delete anything later.
            abandoned.store(true, Ordering::SeqCst);
            empty(Outcome::Refused(format!("the Recycle Bin move did not finish within {} s", wait.as_secs())))
        }
        Err(_) => empty(Outcome::Failed("Recycle Bin worker thread failed".into())),
    }
}

/// [`recycle_files_within`] with [`WAIT`].
pub fn recycle_files(files: &[PathBuf]) -> Report {
    recycle_files_within(files, WAIT)
}

#[cfg(test)]
#[path = "win_fileop_tests.rs"]
mod tests;
