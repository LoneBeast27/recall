//! The drive side of a clip fill: one reader, its unbuffered files and aligned buffer.
use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH};
use windows::Win32::System::IO::DeviceIoControl;

use super::{ALIGN, CHUNK};
use super::super::lane;

const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;

pub(super) struct Reader {
    input: File,
    pub(super) output: File,
    buffer: AlignedBuffer,
    /// Chunk read last: the next one needs no seek when it follows.
    last: Option<u64>,
    started: Instant,
    read: u64,
}

impl Reader {
    pub(super) fn new(input: File, output: File) -> Self {
        Self { input, output, buffer: AlignedBuffer::new(CHUNK as usize), last: None, started: Instant::now(), read: 0 }
    }

    pub(super) fn copy(&mut self, chunk: u64, bytes: u64, throttle: u64, fail_after: u64) -> Result<(), String> {
        if fail_after > 0 && self.read >= fail_after {
            return Err("The clip's drive stopped answering.".into());
        }
        let offset = chunk * CHUNK;
        let want = (bytes - offset).min(CHUNK);
        if self.last.is_none_or(|last| last + 1 != chunk) {
            self.input.seek(SeekFrom::Start(offset)).map_err(|error| format!("seek clip: {error}"))?;
        }
        self.last = Some(chunk);
        let padded = want.div_ceil(ALIGN) * ALIGN;
        let mut got = 0usize;
        while (got as u64) < want {
            let read = self.input.read(&mut self.buffer.bytes()[got..padded as usize]).map_err(|error| format!("read clip: {error}"))?;
            if read == 0 {
                return Err("clip ended early".into());
            }
            got += read;
            if (got as u64) < want && got as u64 % ALIGN != 0 {
                return Err("short unaligned read".into());
            }
        }
        self.buffer.bytes()[got..padded as usize].fill(0);
        self.output.seek(SeekFrom::Start(offset)).map_err(|error| format!("seek fill: {error}"))?;
        self.output.write_all(&self.buffer.bytes()[..padded as usize]).map_err(|error| format!("write fill: {error}"))?;
        lane::count_read(want);
        self.read += want;
        if throttle > 0 {
            let due = Duration::from_secs_f64(self.read as f64 / throttle as f64);
            let elapsed = self.started.elapsed();
            if due > elapsed {
                std::thread::sleep(due - elapsed);
            }
        }
        Ok(())
    }
}

pub(super) fn open_unbuffered(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).custom_flags(FILE_FLAG_NO_BUFFERING.0).open(path)
}

pub(super) fn open_part(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).custom_flags(FILE_FLAG_NO_BUFFERING.0 | FILE_FLAG_WRITE_THROUGH.0).open(path)
}

pub(super) fn set_sparse(file: &File) -> Result<(), String> {
    let handle = HANDLE(file.as_raw_handle());
    let mut returned = 0u32;
    unsafe { DeviceIoControl(handle, FSCTL_SET_SPARSE, None, 0, None, 0, Some(&mut returned), None) }.map_err(|error| format!("make fill sparse: {error}"))
}

/// 4 KiB-aligned heap buffer for unbuffered I/O.
pub struct AlignedBuffer {
    ptr: *mut u8,
    layout: Layout,
}

// The buffer is plain memory owned by one user at a time.
unsafe impl Send for AlignedBuffer {}

impl AlignedBuffer {
    pub fn new(len: usize) -> Self {
        let layout = Layout::from_size_align(len, ALIGN as usize).expect("aligned layout");
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            handle_alloc_error(layout);
        }
        Self { ptr, layout }
    }

    pub fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) };
    }
}
