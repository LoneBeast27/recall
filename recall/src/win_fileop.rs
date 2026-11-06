//! Send files to the Recycle Bin through `IFileOperation`, with a progress
//! sink that refuses any delete Windows would not recycle. Shared by the
//! storage limit (`recall-hdr`) and the library's delete (`recall-native`),
//! which include this file by path; `win_recycle.rs`'s precheck still runs
//! first as a second guard.
//!
//! - Flags: `FOFX_RECYCLEONDELETE | FOF_ALLOWUNDO | FOF_NOCONFIRMATION |
//!   FOF_SILENT | FOF_NOERRORUI`. `FOF_NOCONFIRMATION` answers Windows'
//!   confirmations with "yes", including "delete permanently?". That is safe
//!   only because the sink sees every item before it goes:
//!   `IFileOperationProgressSink::PreDeleteItem` gets `TSF_DELETE_RECYCLE_IF_POSSIBLE`
//!   in `dwFlags` when the item will go to the Recycle Bin, and the sink
//!   returns `E_ABORT` whenever it is missing, which cancels that delete
//!   and the rest of the operation ([`allows_delete`]).
//! - Threading: `IFileOperation` is documented for single-threaded
//!   apartments only, so every call runs on its own short-lived thread that
//!   initialises COM as an STA (`COINIT_APARTMENTTHREADED`) and uninitialises
//!   it before it ends. The caller's thread and COM state are not touched.

use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use windows::core::{implement, Ref, HRESULT, PCWSTR};
use windows::Win32::Foundation::E_ABORT;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem, SHCreateItemFromParsingName,
    FILEOPERATION_FLAGS, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
};

/// `TSF_DELETE_RECYCLE_IF_POSSIBLE`: the item will go to the Recycle Bin.
pub const TSF_DELETE_RECYCLE_IF_POSSIBLE: u32 = 0x80;

/// The flags every Recall `IFileOperation` delete uses.
pub const OP_FLAGS: u32 = FOFX_RECYCLEONDELETE.0 | FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0;

/// The sink's decision: a delete goes ahead only when Windows says it recycles.
pub fn allows_delete(dwflags: u32) -> bool {
    dwflags & TSF_DELETE_RECYCLE_IF_POSSIBLE != 0
}

/// How one recycle call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Recycled,
    /// Windows would have deleted at least one item for good: the sink
    /// refused it (or the operation was cancelled). Nothing was deleted for good.
    Refused(String),
    Failed(String),
}

/// The outcome plus every `dwFlags` the sink saw (for the record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub outcome: Outcome,
    pub flags_seen: Vec<u32>,
}

#[derive(Default)]
struct Seen {
    flags: Vec<u32>,
    refused: bool,
}

#[implement(IFileOperationProgressSink)]
struct RecycleGuard {
    seen: Arc<Mutex<Seen>>,
}

impl IFileOperationProgressSink_Impl for RecycleGuard_Impl {
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
    fn PreDeleteItem(&self, dwflags: u32, _: Ref<IShellItem>) -> windows::core::Result<()> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.flags.push(dwflags);
        if allows_delete(dwflags) {
            return Ok(());
        }
        seen.refused = true;
        Err(E_ABORT.into())
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

/// The outcome from what the sink saw and what the operation returned.
pub fn classify(refused: bool, perform: Result<(), u32>, aborted: bool) -> Outcome {
    if refused {
        return Outcome::Refused("Windows would have deleted it for good instead of moving it to the Recycle Bin".into());
    }
    match perform {
        Err(code) if code == E_ABORT.0 as u32 || CANCELLED.contains(&code) => Outcome::Refused(format!("the Recycle Bin move was cancelled ({code:#x})")),
        Err(code) => Outcome::Failed(format!("Recycle Bin move failed ({code:#x})")),
        Ok(()) if aborted => Outcome::Refused("the Recycle Bin move was cancelled".into()),
        Ok(()) => Outcome::Recycled,
    }
}

fn wide(path: &std::path::Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// One operation on the current (STA) thread.
fn perform(files: &[PathBuf]) -> Report {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let result = (|| -> windows::core::Result<(Result<(), u32>, bool)> {
        // SAFETY: COM is initialised as an STA on this thread by the caller.
        let op: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL)? };
        unsafe { op.SetOperationFlags(FILEOPERATION_FLAGS(OP_FLAGS))? };
        let sink: IFileOperationProgressSink = RecycleGuard { seen: seen.clone() }.into();
        for file in files {
            let path = wide(file);
            // SAFETY: a NUL-terminated path alive for the call.
            let item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None)? };
            unsafe { op.DeleteItem(&item, &sink)? };
        }
        let performed = unsafe { op.PerformOperations() }.map_err(|e| e.code().0 as u32);
        let aborted = unsafe { op.GetAnyOperationsAborted() }.map(|b| b.as_bool()).unwrap_or(false);
        Ok((performed, aborted))
    })();
    let seen = seen.lock().unwrap_or_else(|p| p.into_inner());
    let outcome = match result {
        Ok((performed, aborted)) => classify(seen.refused, performed, aborted),
        Err(e) if seen.refused => classify(true, Err(e.code().0 as u32), false),
        Err(e) => Outcome::Failed(format!("Recycle Bin move could not start: {e}")),
    };
    Report { outcome, flags_seen: seen.flags.clone() }
}

/// Recycle `files` in one operation on a fresh STA thread (see the module doc).
pub fn recycle_files(files: &[PathBuf]) -> Report {
    if files.is_empty() {
        return Report { outcome: Outcome::Recycled, flags_seen: Vec::new() };
    }
    let files = files.to_vec();
    let worker = std::thread::Builder::new().name("recall-recycle".into()).spawn(move || {
        // SAFETY: this thread owns its apartment; it is uninitialised below.
        let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if init.is_err() {
            return Report { outcome: Outcome::Failed(format!("COM did not start ({:#x})", init.0 as u32)), flags_seen: Vec::new() };
        }
        let report = perform(&files);
        // SAFETY: matches the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
        report
    });
    match worker.map(|w| w.join()) {
        Ok(Ok(report)) => report,
        _ => Report { outcome: Outcome::Failed("Recycle Bin worker thread failed".into()), flags_seen: Vec::new() },
    }
}

#[cfg(test)]
#[path = "win_fileop_tests.rs"]
mod tests;
