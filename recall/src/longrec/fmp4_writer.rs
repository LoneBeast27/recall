//! [`FileWriter`]: buffers the fragmented-MP4 muxer's output and writes it in
//! large sequential chunks that end on fragment boundaries (see `fmp4`).

use std::io::Write;
use std::time::{Duration, Instant};

/// Smallest disk write (except the header, the timed flush and the tail).
pub(crate) const FLUSH_AT: usize = 4 << 20;
/// A single fragment bigger than this is written mid-fragment.
const HARD_CAP: usize = 64 << 20;
/// A fragment boundary this long after the last write goes out even below
/// [`FLUSH_AT`], so a crash on a quiet (low-bitrate) scene loses seconds,
/// not minutes.
pub(crate) const FLUSH_EVERY: Duration = Duration::from_secs(5);

/// Buffers FFmpeg's output and writes it out in large sequential chunks that
/// end on fragment boundaries. Generic so the policy is unit-tested.
pub(crate) struct FileWriter<W: Write> {
    out: W,
    buf: Vec<u8>,
    /// `buf[..safe]` ends on a fragment boundary.
    safe: usize,
    bytes: u64,
    error: Option<String>,
    /// A callback this long was cut by a full buffer, not a flush point.
    chunk: usize,
    /// When the disk was last written (the timed flush).
    last_write: Instant,
}

impl<W: Write> FileWriter<W> {
    pub fn new(out: W, chunk: usize) -> Self {
        FileWriter { out, buf: Vec::new(), safe: 0, bytes: 0, error: None, chunk, last_write: Instant::now() }
    }

    /// One AVIO callback's bytes.
    pub fn push(&mut self, data: &[u8]) {
        self.push_at(data, Instant::now());
    }

    /// [`Self::push`] at `now`: a fragment boundary goes to disk once 4 MiB
    /// are buffered or [`FLUSH_EVERY`] passed since the last write.
    pub fn push_at(&mut self, data: &[u8], now: Instant) {
        if self.error.is_some() {
            return;
        }
        self.buf.extend_from_slice(data);
        if data.len() < self.chunk {
            self.safe = self.buf.len();
            if self.buf.len() >= FLUSH_AT || now.saturating_duration_since(self.last_write) >= FLUSH_EVERY {
                self.write_out(self.buf.len(), now);
            }
        } else if self.buf.len() >= HARD_CAP {
            self.write_out(self.buf.len(), now);
        }
    }

    /// Write everything buffered now (the header, so a crash keeps it).
    pub fn flush_now(&mut self) {
        self.write_out(self.buf.len(), Instant::now());
    }

    fn write_out(&mut self, n: usize, now: Instant) {
        if n == 0 || self.error.is_some() {
            return;
        }
        match self.out.write_all(&self.buf[..n]) {
            Ok(()) => {
                self.bytes += n as u64;
                self.buf.drain(..n);
                self.safe = self.safe.saturating_sub(n);
                self.last_write = now;
            }
            Err(e) => self.error = Some(format!("write recording: {e}")),
        }
    }

    /// Finalize: the rest of the buffer, then flush. Returns bytes written.
    pub fn finish(&mut self) -> Result<u64, String> {
        self.flush_now();
        if let Err(e) = self.out.flush() {
            self.error.get_or_insert(format!("flush recording: {e}"));
        }
        match &self.error {
            Some(e) => Err(e.clone()),
            None => Ok(self.bytes),
        }
    }

    /// Close early: keep only complete fragments.
    pub fn abandon(&mut self) {
        self.write_out(self.safe, Instant::now());
        self.buf.clear();
        self.safe = 0;
        let _ = self.out.flush();
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Bytes buffered but not yet written.
    pub fn buffered(&self) -> &[u8] {
        &self.buf
    }

    #[cfg(test)]
    pub fn into_inner(self) -> W {
        self.out
    }
}

impl<W: Write + std::io::Seek> FileWriter<W> {
    /// Overwrite `bytes` at `offset` of what is already written (one small
    /// in-place write, used once per recording for the duration field).
    pub fn patch(&mut self, offset: u64, bytes: &[u8]) -> Result<(), String> {
        use std::io::SeekFrom;
        if offset + bytes.len() as u64 > self.bytes {
            return Err("patch beyond the written data".into());
        }
        self.out
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.out.write_all(bytes))
            .and_then(|_| self.out.seek(SeekFrom::End(0)).map(|_| ()))
            .map_err(|e| format!("patch recording header: {e}"))
    }
}
