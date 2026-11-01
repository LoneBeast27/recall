//! The clip as Media Foundation sees it while it is still loading: an `IMFByteStream` over the
//! SSD fill (`services::storage::fill`). A read of a part that has not arrived waits for it, so
//! the player and the sound decoder never know the file is incomplete.
use std::collections::HashMap;
use std::fs::File;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{E_ABORT, E_INVALIDARG, E_NOTIMPL, E_POINTER};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncResult, IMFByteStream, IMFByteStream_Impl, IMFMediaSource, MF_OBJECT_INVALID,
    MF_RESOLUTION_MEDIASOURCE, MF_RESOLUTION_READ, MFBYTESTREAM_IS_READABLE, MFBYTESTREAM_IS_SEEKABLE,
    MFBYTESTREAM_SEEK_ORIGIN, MFCreateAsyncResult, MFCreateSourceResolver, MFInvokeCallback, msoBegin, msoCurrent,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::core::{IUnknown, Interface, PCWSTR, Ref};
use windows_core::{BOOL, implement};

use crate::services::storage::fill::{AlignedBuffer, ClipFill};

const READ_WINDOW: usize = 1 << 20;

struct Inner {
    fill: ClipFill,
    /// The player pulls the copy to where it reads; the sound decoder only follows.
    want: bool,
    file: File,
    scratch: Mutex<AlignedBuffer>,
    position: AtomicU64,
    /// Bytes read per finished `BeginRead`, until its `EndRead`.
    finished: Mutex<HashMap<usize, u32>>,
}

impl Inner {
    /// Reads into `out` at `offset`, waiting for the bytes. None when the fill stopped.
    fn read(&self, offset: u64, out: &mut [u8]) -> Option<usize> {
        let length = (out.len() as u64).min(self.fill.bytes().saturating_sub(offset)) as usize;
        if length == 0 {
            return Some(0);
        }
        if !self.fill.wait_for(offset, offset + length as u64, self.want) {
            return self.stopped(&mut out[..length]);
        }
        let mut scratch = self.scratch.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut done = 0;
        while done < length {
            let window = (length - done).min(READ_WINDOW - 8192);
            match self.fill.read_at(&self.file, offset + done as u64, &mut out[done..done + window], &mut scratch) {
                Ok(0) | Err(_) if self.fill.is_direct() => {
                    drop(scratch);
                    self.fill.lost("The clip's drive stopped answering.".into());
                    return self.stopped(&mut out[..length]);
                }
                Ok(0) | Err(_) => break,
                Ok(read) => done += read,
            }
        }
        Some(done)
    }

    /// This read will never be served. An abort would end the player and everything that loaded
    /// with it, so it waits, and a seek must not wait behind it: reads are served one at a time,
    /// and Media Foundation finishes one before it takes the next. Nothing plays these zeros: the
    /// editor stops playback before the hole, and the seek drops them.
    fn stopped(&self, out: &mut [u8]) -> Option<usize> {
        if !self.fill.stays_open() || !self.fill.wait_released() {
            return None;
        }
        out.fill(0);
        Some(out.len())
    }
}

/// One pending read. The buffer and the result stay alive until the callback runs.
struct ReadJob {
    data: *mut u8,
    length: usize,
    offset: u64,
    result: IMFAsyncResult,
}
unsafe impl Send for ReadJob {}

#[implement(IMFByteStream)]
struct FillStream {
    inner: Arc<Inner>,
    reads: Sender<ReadJob>,
}

fn run_reads(inner: Arc<Inner>, jobs: mpsc::Receiver<ReadJob>) {
    while let Ok(job) = jobs.recv() {
        let out = unsafe { std::slice::from_raw_parts_mut(job.data, job.length) };
        let read = inner.read(job.offset, out);
        if let Some(count) = read {
            inner.position.store(job.offset + count as u64, Ordering::Release);
            inner.finished.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(job.result.as_raw() as usize, count as u32);
        }
        unsafe {
            let _ = job.result.SetStatus(if read.is_some() { windows::core::HRESULT(0) } else { E_ABORT });
            let _ = MFInvokeCallback(&job.result);
        }
    }
}

#[allow(non_snake_case)]
impl IMFByteStream_Impl for FillStream_Impl {
    fn GetCapabilities(&self) -> windows_core::Result<u32> {
        Ok(MFBYTESTREAM_IS_READABLE | MFBYTESTREAM_IS_SEEKABLE)
    }

    fn GetLength(&self) -> windows_core::Result<u64> {
        Ok(self.inner.fill.bytes())
    }

    fn SetLength(&self, _: u64) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn GetCurrentPosition(&self) -> windows_core::Result<u64> {
        Ok(self.inner.position.load(Ordering::Acquire))
    }

    fn SetCurrentPosition(&self, position: u64) -> windows_core::Result<()> {
        self.inner.position.store(position, Ordering::Release);
        Ok(())
    }

    fn IsEndOfStream(&self) -> windows_core::Result<BOOL> {
        Ok((self.inner.position.load(Ordering::Acquire) >= self.inner.fill.bytes()).into())
    }

    fn Read(&self, data: *mut u8, length: u32, read: *mut u32) -> windows_core::Result<()> {
        if data.is_null() || read.is_null() {
            return Err(E_POINTER.into());
        }
        let out = unsafe { std::slice::from_raw_parts_mut(data, length as usize) };
        let offset = self.inner.position.load(Ordering::Acquire);
        let count = self.inner.read(offset, out).ok_or(windows_core::Error::from(E_ABORT))?;
        self.inner.position.store(offset + count as u64, Ordering::Release);
        unsafe { *read = count as u32 };
        Ok(())
    }

    fn BeginRead(&self, data: *mut u8, length: u32, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> windows_core::Result<()> {
        if data.is_null() {
            return Err(E_POINTER.into());
        }
        let result = unsafe { MFCreateAsyncResult(None, callback.as_ref(), state.as_ref()) }?;
        let offset = self.inner.position.load(Ordering::Acquire);
        self.reads.send(ReadJob { data, length: length as usize, offset, result }).map_err(|_| windows_core::Error::from(E_ABORT))
    }

    fn EndRead(&self, result: Ref<IMFAsyncResult>) -> windows_core::Result<u32> {
        let result = result.ok()?;
        unsafe { result.GetStatus() }?;
        Ok(self.inner.finished.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&(result.as_raw() as usize)).unwrap_or(0))
    }

    fn Write(&self, _: *const u8, _: u32) -> windows_core::Result<u32> {
        Err(E_NOTIMPL.into())
    }

    fn BeginWrite(&self, _: *const u8, _: u32, _: Ref<IMFAsyncCallback>, _: Ref<IUnknown>) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EndWrite(&self, _: Ref<IMFAsyncResult>) -> windows_core::Result<u32> {
        Err(E_NOTIMPL.into())
    }

    fn Seek(&self, origin: MFBYTESTREAM_SEEK_ORIGIN, offset: i64, _: u32) -> windows_core::Result<u64> {
        let base = if origin == msoBegin {
            0
        } else if origin == msoCurrent {
            self.inner.position.load(Ordering::Acquire) as i64
        } else {
            return Err(E_INVALIDARG.into());
        };
        let target = base.saturating_add(offset).max(0) as u64;
        self.inner.position.store(target, Ordering::Release);
        Ok(target)
    }

    fn Flush(&self) -> windows_core::Result<()> {
        Ok(())
    }

    fn Close(&self) -> windows_core::Result<()> {
        Ok(())
    }
}

fn inner(fill: &ClipFill, want: bool) -> Result<Inner, String> {
    let file = fill.open_reader().map_err(|error| format!("open clip fill: {error}"))?;
    Ok(Inner {
        fill: fill.clone(),
        want,
        file,
        scratch: Mutex::new(AlignedBuffer::new(READ_WINDOW)),
        position: AtomicU64::new(0),
        finished: Mutex::new(HashMap::new()),
    })
}

fn byte_stream(fill: &ClipFill, want: bool) -> Result<IMFByteStream, String> {
    let inner = Arc::new(inner(fill, want)?);
    let (reads, jobs) = mpsc::channel();
    let worker = Arc::clone(&inner);
    std::thread::Builder::new().name("recall-clip-read".into()).spawn(move || run_reads(worker, jobs)).map_err(|error| format!("start clip reader: {error}"))?;
    Ok(FillStream { inner, reads }.into())
}

/// A Media Foundation source over `fill`. Reads the clip's index, so the index must be there
/// (or `fill` running). Blocks: call it off the UI thread.
pub(crate) fn open_source(fill: &ClipFill, want: bool) -> Result<IMFMediaSource, String> {
    let stream = byte_stream(fill, want)?;
    let hint: Vec<u16> = "clip.mp4".encode_utf16().chain(Some(0)).collect();
    let mut kind = MF_OBJECT_INVALID;
    let mut object: Option<IUnknown> = None;
    unsafe {
        let resolver = MFCreateSourceResolver().map_err(|error| format!("clip source: {error}"))?;
        resolver
            .CreateObjectFromByteStream(&stream, PCWSTR(hint.as_ptr()), (MF_RESOLUTION_MEDIASOURCE.0 | MF_RESOLUTION_READ.0) as u32, None::<&IPropertyStore>, &mut kind, &mut object)
            .map_err(|error| format!("clip source: {error}"))?;
    }
    object.ok_or("clip source: nothing resolved")?.cast().map_err(|error| format!("clip source: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::storage::fill::Outcome;

    #[test]
    fn a_failed_read_of_a_clip_on_its_drive_says_so_and_a_seek_frees_the_queue() {
        let dir = std::env::temp_dir().join(format!("recall-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("clip.mp4");
        std::fs::write(&source, vec![7u8; 1 << 20]).unwrap();
        let metadata = source.metadata().unwrap();
        let fill = ClipFill::direct(&source, 1 << 20, metadata.modified().unwrap(), 1 << 19);
        let reader = Arc::new(inner(&fill, true).unwrap());
        let mut near = vec![0u8; 4096];
        assert_eq!(reader.read(0, &mut near), Some(4096));
        assert_eq!(near[0], 7);
        assert_eq!(fill.outcome(), Outcome::Done);
        // Not open in the player yet: the open fails rather than shows a clip that cannot play.
        assert_eq!(reader.read(1 << 19, &mut near), None);
        assert_eq!(fill.outcome(), Outcome::Failed);
        assert!(fill.error().is_some());
        fill.set_playing();
        let parked = {
            let reader = Arc::clone(&reader);
            std::thread::spawn(move || reader.read(0, &mut [1u8; 4096]))
        };
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!parked.is_finished(), "a read of what will not come waits, it does not abort");
        fill.note_seek();
        assert_eq!(parked.join().unwrap(), Some(4096), "a seek gets the queue back");
        fill.cancel();
        assert_eq!(reader.read(0, &mut near), None, "closing ends it");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
