//! The live RTSS client: maps `RTSSSharedMemoryV2` each poll (a restarted
//! RTSS is picked up), turns on RTSS's statistics for the stats target
//! (`STATFLAG_RECORD`, the way MSI Afterburner does) when the mapping allows
//! writing, and turns them off again for an entry it switched on.

use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP, FILE_MAP_READ, FILE_MAP_WRITE,
    MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
};
use windows::Win32::System::SystemInformation::GetTickCount;

use super::rtss::{choose_access, parse, record_plan, Access, Mem, RecordPlan, RtssRead, STATFLAG_RECORD};

#[derive(Default)]
pub struct RtssClient {
    /// Decided on the first poll that finds the mapping; a refused write is
    /// not retried for this client's life.
    access: Option<Access>,
    /// The pid whose `STATFLAG_RECORD` we set.
    owned: Option<u32>,
}

impl RtssClient {
    pub fn access(&self) -> Option<Access> {
        self.access
    }

    /// Reads `pid`'s framerate; with write access, first makes sure RTSS
    /// records statistics for `record` (the stats target) and only for it.
    pub fn poll(&mut self, pid: Option<u32>, record: Option<u32>) -> RtssRead {
        let Some(view) = self.open() else { return RtssRead::NoMemory };
        if view.writable {
            let plan = record_plan(&view, record, self.owned);
            view.apply(&plan);
            self.owned = plan.owned;
        }
        parse(&view, pid, unsafe { GetTickCount() })
    }

    fn open(&mut self) -> Option<View> {
        if self.access != Some(Access::ReadOnly) {
            if let Some(view) = View::open(true) {
                self.decide(choose_access(true, true));
                return Some(view);
            }
        }
        let view = View::open(false);
        if self.access.is_none() {
            self.decide(choose_access(false, view.is_some()));
        }
        view
    }

    /// Records (once) which path is in use.
    fn decide(&mut self, access: Option<Access>) {
        if access.is_some() && self.access != access {
            eprintln!("recall-overlay: RTSS shared memory opened {access:?}");
            self.access = access;
        }
    }
}

impl Drop for RtssClient {
    /// Stats off (or the overlay closing): switch off what we switched on.
    fn drop(&mut self) {
        if self.owned.is_none() {
            return;
        }
        if let Some(view) = View::open(true) {
            view.apply(&record_plan(&view, None, self.owned));
        }
    }
}

/// A mapped view of RTSS's memory; another process writes it, so every read
/// is a volatile copy and every write an atomic bit operation.
struct View {
    handle: HANDLE,
    address: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    writable: bool,
}

impl View {
    fn open(writable: bool) -> Option<View> {
        let access = if writable { FILE_MAP(FILE_MAP_READ.0 | FILE_MAP_WRITE.0) } else { FILE_MAP_READ };
        unsafe {
            let handle = OpenFileMappingW(access.0, false, w!("RTSSSharedMemoryV2")).ok()?;
            let address = MapViewOfFile(handle, access, 0, 0, 0);
            if address.Value.is_null() {
                let _ = CloseHandle(handle);
                return None;
            }
            let mut info = MEMORY_BASIC_INFORMATION::default();
            let queried =
                VirtualQuery(Some(address.Value as *const _), &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
            let len = if queried == 0 { 0 } else { info.RegionSize };
            Some(View { handle, address, len, writable })
        }
    }

    fn flag_word(&self, offset: usize) -> Option<&AtomicU32> {
        if !self.writable || !offset.is_multiple_of(4) || offset.checked_add(4)? > self.len {
            return None;
        }
        // Page-aligned view, 4-aligned field, inside the mapping, mapped writable.
        Some(unsafe { AtomicU32::from_ptr(self.address.Value.cast::<u8>().add(offset).cast::<u32>()) })
    }

    fn apply(&self, plan: &RecordPlan) {
        if let Some(word) = plan.clear.and_then(|at| self.flag_word(at)) {
            word.fetch_and(!STATFLAG_RECORD, Ordering::SeqCst);
        }
        if let Some(word) = plan.set.and_then(|at| self.flag_word(at)) {
            word.fetch_or(STATFLAG_RECORD, Ordering::SeqCst);
        }
    }
}

impl Mem for View {
    fn len(&self) -> usize {
        self.len
    }

    fn u32_at(&self, offset: usize) -> Option<u32> {
        if !offset.is_multiple_of(4) || offset.checked_add(4)? > self.len {
            return None;
        }
        Some(unsafe { std::ptr::read_volatile(self.address.Value.cast::<u8>().add(offset).cast::<u32>()) })
    }
}

impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.address);
            let _ = CloseHandle(self.handle);
        }
    }
}
