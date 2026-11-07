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
//! thread still running cannot delete anything afterwards. Such a thread
//! holds the one [`STUCK`] slot until it ends: while it is held every new call
//! is refused at once (no new thread), so a dialog nobody answers can never
//! pile up stuck threads.
//!
//! Private desktop (a guard, never a way to delete): before any COM or shell
//! call the worker thread moves to its own desktop that is never shown
//! (`CreateDesktopW` with a unique `RecallRecycle<pid>-<n>` name and only the
//! rights in [`DESKTOP_ACCESS`], then `SetThreadDesktop`). Any dialog Windows
//! raises for the call, such as its "Delete File" question for a file too big
//! for the bin, lands there, where nobody can see or answer it, so it can only
//! end in the [`WAIT`] refusal. If the desktop cannot be made or set, the call
//! is refused before anything is touched; it never falls back to the visible
//! desktop. `CloseDesktop` fails while a thread is still on the desktop
//! (ERROR_BUSY), so a keeper thread closes it after the worker ends; a worker
//! stuck on a dialog keeps it open until it ends or the process exits.

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

/// Rights on the private desktop: create the windows and menus COM and the
/// shell make for the call, and read/write its own objects. No switching,
/// hook or journal rights.
pub const DESKTOP_ACCESS: u32 = 0x0001 | 0x0002 | 0x0004 | 0x0080; // READOBJECTS | CREATEWINDOW | CREATEMENU | WRITEOBJECTS

#[link(name = "user32", kind = "raw-dylib")]
unsafe extern "system" {
    // Desktop handles as isize, as record_screen.rs declares CloseDesktop in recall-hdr.
    fn CreateDesktopW(name: *const u16, device: *const u16, devmode: *const std::ffi::c_void, flags: u32, access: u32, sa: *const std::ffi::c_void) -> isize;
    fn CloseDesktop(desktop: isize) -> i32;
    fn SetThreadDesktop(desktop: isize) -> i32;
}

/// How the worker gets its private desktop (production: [`REAL_DESKTOP`]; tests inject failures).
#[derive(Clone, Copy)]
pub struct DesktopFns {
    pub create: fn(&str) -> Option<isize>,
    pub set: fn(isize) -> bool,
    pub close: fn(isize),
}

fn create_desktop(name: &str) -> Option<isize> {
    let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: a NUL-terminated name; default device, mode and security.
    let desk = unsafe { CreateDesktopW(name.as_ptr(), std::ptr::null(), std::ptr::null(), 0, DESKTOP_ACCESS, std::ptr::null()) };
    (desk != 0).then_some(desk)
}

fn set_desktop(desk: isize) -> bool {
    // SAFETY: a desktop handle from CreateDesktopW; the calling thread has no windows yet.
    unsafe { SetThreadDesktop(desk) != 0 }
}

fn close_desktop(desk: isize) {
    // SAFETY: a desktop handle from CreateDesktopW, closed once, after its thread ended.
    unsafe { CloseDesktop(desk) };
}

pub const REAL_DESKTOP: DesktopFns = DesktopFns { create: create_desktop, set: set_desktop, close: close_desktop };

static DESKTOPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// The name of the last private desktop (the live probe looks at it).
pub static LAST_DESKTOP: Mutex<String> = Mutex::new(String::new());

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
    /// Items Windows reported done (`PostDeleteItem` succeeded), counted as each went.
    pub recycled: usize,
}

#[derive(Default)]
struct Seen {
    flags: Vec<u32>,
    calls: (usize, usize),
    paths: HashSet<String>,
    done: HashSet<String>,
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
    fn PostDeleteItem(&self, _: u32, psiitem: Ref<IShellItem>, hrdelete: HRESULT, _: Ref<IShellItem>) -> windows::core::Result<()> {
        if let Some(path) = item_path(&psiitem).filter(|_| hrdelete.is_ok()) {
            self.seen.lock().unwrap_or_else(|p| p.into_inner()).done.insert(path);
        }
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
        let queued = (|| -> windows::core::Result<()> {
            for file in files {
                let path = wide(file);
                // SAFETY: a NUL-terminated path alive for the call.
                let item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None)? };
                unsafe { op.DeleteItem(&item, &per_item)? };
            }
            Ok(())
        })();
        let run = queued.map(|()| {
            let performed = unsafe { op.PerformOperations() }.map_err(|e| e.code().0 as u32);
            let aborted = unsafe { op.GetAnyOperationsAborted() }.map(|b| b.as_bool()).unwrap_or(false);
            (performed, aborted)
        });
        // Unadvised on every path once advised, early errors included.
        let _ = unsafe { op.Unadvise(cookie) };
        run
    })();
    let seen = seen.lock().unwrap_or_else(|p| p.into_inner());
    let items = (seen.paths.len(), files.len());
    let outcome = match result {
        Ok((performed, aborted)) => classify(seen.refused, items, performed, aborted),
        Err(e) if seen.refused => classify(true, items, Err(e.code().0 as u32), false),
        Err(e) => Outcome::Failed(format!("Recycle Bin move could not start: {e}")),
    };
    Report { outcome, flags_seen: seen.flags.clone(), calls: seen.calls, items, recycled: seen.done.len() }
}

/// Recycle `files` in one operation on a fresh STA thread, waiting at most `wait`.
pub fn recycle_files_within(files: &[PathBuf], wait: Duration) -> Report {
    recycle_with_flags(files, wait, OP_FLAGS)
}

/// Held by a worker that outlived its caller's wait (a dialog nobody answers) until it ends.
pub static STUCK: AtomicBool = AtomicBool::new(false);
/// Worker threads started (tests check that a refused call starts none).
pub static SPAWNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The refusal while an earlier call is stuck.
pub const STUCK_REFUSAL: &str = "an earlier Recycle Bin move is still waiting on Windows; nothing was moved";

/// [`recycle_files_within`] with another flag set (the live probe compares; production uses [`OP_FLAGS`]).
fn recycle_with_flags(files: &[PathBuf], wait: Duration, flags: u32) -> Report {
    recycle_with_slot(files, wait, flags, &STUCK, &SPAWNED, REAL_DESKTOP)
}

/// The call, with its stuck slot and thread counter (tests use their own).
fn recycle_with_slot(
    files: &[PathBuf],
    wait: Duration,
    flags: u32,
    stuck: &'static AtomicBool,
    spawned_count: &'static std::sync::atomic::AtomicUsize,
    desktop: DesktopFns,
) -> Report {
    let empty = |outcome| Report { outcome, flags_seen: Vec::new(), calls: (0, 0), items: (0, files.len()), recycled: 0 };
    if files.is_empty() {
        return empty(Outcome::Recycled);
    }
    if stuck.load(Ordering::SeqCst) {
        return empty(Outcome::Refused(STUCK_REFUSAL.into()));
    }
    // 0 running, 1 the worker finished in time, 2 the caller gave up: exactly one side wins.
    const RUNNING: u8 = 0;
    const DONE: u8 = 1;
    const GAVE_UP: u8 = 2;
    let state = Arc::new(std::sync::atomic::AtomicU8::new(RUNNING));
    let (tx, rx) = std::sync::mpsc::channel();
    let abandoned = Arc::new(AtomicBool::new(false));
    let (list, flag, worker_state) = (files.to_vec(), abandoned.clone(), state.clone());
    spawned_count.fetch_add(1, Ordering::SeqCst);
    let name = format!("RecallRecycle{}-{}", std::process::id(), DESKTOPS.fetch_add(1, Ordering::SeqCst));
    *LAST_DESKTOP.lock().unwrap_or_else(|p| p.into_inner()) = name.clone();
    let refusal = |why: &str, n: usize| Report { outcome: Outcome::Refused(why.into()), flags_seen: Vec::new(), calls: (0, 0), items: (0, n), recycled: 0 };
    let spawned = std::thread::Builder::new().name("recall-recycle-keeper".into()).spawn(move || {
        let finish = |report: Report| {
            if worker_state.compare_exchange(RUNNING, DONE, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                // The caller gave up on this call and took the stuck slot for it: release it now.
                stuck.store(false, Ordering::SeqCst);
            }
            let _ = tx.send(report);
        };
        let Some(desk) = (desktop.create)(&name) else {
            return finish(refusal("no private desktop for the Recycle Bin move; nothing was touched", list.len()));
        };
        let worker = std::thread::Builder::new().name("recall-recycle".into()).spawn(move || {
            // Before any COM or shell call: every window of this call lands on the private desktop.
            if !(desktop.set)(desk) {
                return refusal("could not move the Recycle Bin call to its private desktop; nothing was touched", list.len());
            }
            // SAFETY: this thread owns its apartment; it is uninitialised below.
            let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            if init.is_err() {
                return Report { outcome: Outcome::Failed(format!("COM did not start ({:#x})", init.0 as u32)), flags_seen: Vec::new(), calls: (0, 0), items: (0, list.len()), recycled: 0 };
            }
            let report = perform(&list, flag, flags);
            // SAFETY: matches the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
            report
        });
        let report = match worker.map(|w| w.join()) {
            Ok(Ok(report)) => report,
            _ => Report { outcome: Outcome::Failed("Recycle Bin worker thread failed".into()), flags_seen: Vec::new(), calls: (0, 0), items: (0, 0), recycled: 0 },
        };
        // The worker has left the desktop now that it ended: only then can it close.
        (desktop.close)(desk);
        finish(report);
    });
    if spawned.is_err() {
        return empty(Outcome::Failed("Recycle Bin worker thread failed to start".into()));
    }
    match rx.recv_timeout(wait) {
        Ok(report) => report,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // Take the slot first, so the worker can only release it after it was taken.
            stuck.store(true, Ordering::SeqCst);
            // The sink refuses from now on, before the race below is even decided.
            abandoned.store(true, Ordering::SeqCst);
            if state.compare_exchange(RUNNING, GAVE_UP, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                // The worker finished at the deadline after all: keep its answer.
                stuck.store(false, Ordering::SeqCst);
                return rx.recv().unwrap_or_else(|_| empty(Outcome::Failed("Recycle Bin worker thread failed".into())));
            }
            // The sink refuses every later delete, so the thread cannot delete anything now.
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
