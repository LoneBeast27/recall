//! The player's side of a clip fill: reading it, and what a read does once the rest of the clip
//! will not arrive.
use std::fs::{File, OpenOptions};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::sync::atomic::Ordering;
use std::time::Duration;

use windows::Win32::Storage::FileSystem::FILE_FLAG_NO_BUFFERING;

use super::{ALIGN, AlignedBuffer, ClipFill, Outcome, RELEASE, lock};

impl ClipFill {
    /// Reads `[offset, ...)` from the SSD file. The range must be there (`wait_for`).
    pub fn read_at(&self, file: &File, offset: u64, out: &mut [u8], scratch: &mut AlignedBuffer) -> std::io::Result<usize> {
        if self.0.direct && self.0.fail_after > 0 && offset >= self.0.fail_after && lock(&self.0).sought.is_some() {
            return Err(std::io::Error::other("test drive gone"));
        }
        let length = (out.len() as u64).min(self.0.bytes.saturating_sub(offset));
        let aligned = offset / ALIGN * ALIGN;
        let skip = (offset - aligned) as usize;
        let span = ((skip as u64 + length).div_ceil(ALIGN) * ALIGN).min(scratch.bytes().len() as u64) as usize;
        let mut got = 0;
        while got < span {
            let read = file.seek_read(&mut scratch.bytes()[got..span], aligned + got as u64)?;
            if read == 0 {
                break;
            }
            got += read;
        }
        let usable = got.saturating_sub(skip).min(length as usize);
        out[..usable].copy_from_slice(&scratch.bytes()[skip..skip + usable]);
        Ok(usable)
    }

    /// A handle for [`ClipFill::read_at`]. A clip read where it lies has no holes to keep out of
    /// the file cache, and the drive's own read-ahead helps it.
    pub fn open_reader(&self) -> std::io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(true).share_mode(7);
        if !self.0.direct {
            options.custom_flags(FILE_FLAG_NO_BUFFERING.0);
        }
        options.open(&self.0.part)
    }

    /// The player has this clip open: from now on a read of a part that will never come waits
    /// for the clip to close, so that everything which did arrive stays playable.
    pub fn set_playing(&self) {
        self.0.playing.store(true, Ordering::Release);
    }

    /// After a failed read: true when the reader should wait for the close.
    pub fn stays_open(&self) -> bool {
        self.0.playing.load(Ordering::Acquire) && lock(&self.0).outcome == Outcome::Failed
    }

    /// The player seeks: a read stopped on what will never arrive must not hold up the seek.
    pub fn note_seek(&self) {
        lock(&self.0).sought = Some(std::time::Instant::now());
        self.0.changed.notify_all();
    }

    /// After a failed read, waits. True: a seek came, so the player needs its queue back. False:
    /// the clip closed.
    pub fn wait_released(&self) -> bool {
        let mut state = lock(&self.0);
        while state.outcome == Outcome::Failed {
            if state.sought.is_some_and(|at| at.elapsed() < RELEASE) {
                return true;
            }
            state = self.0.changed.wait_timeout(state, Duration::from_millis(100)).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
        }
        false
    }

    /// A read of a clip that lies on its drive failed: nothing more can come. A copy finds this
    /// out in its own reader; this one has none, so the readers say.
    pub fn lost(&self, error: String) {
        if !self.0.direct {
            return;
        }
        let mut state = lock(&self.0);
        if state.outcome != Outcome::Done {
            return;
        }
        state.outcome = Outcome::Failed;
        state.error = Some(error);
        state.have.fill(false);
        state.missing = state.have.len() as u64;
        drop(state);
        self.0.revision.fetch_add(1, Ordering::Relaxed);
        self.0.changed.notify_all();
    }
}
