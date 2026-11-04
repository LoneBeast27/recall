//! One VRAM reading 30 s into each recording: by then the D3D11 frames pool and NVENC's
//! queue have grown to their steady size, which the reading at encoder open cannot show.

use std::time::{Duration, Instant};

const AFTER: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct VramWatch {
    opened: Option<Instant>,
}

impl VramWatch {
    /// The encoder (re)opened: read again 30 s from now.
    pub fn opened(&mut self) {
        self.opened = Some(Instant::now());
    }

    /// The log line once 30 s have passed since the encoder opened, then nothing until the next open.
    pub fn due(&mut self) -> Option<String> {
        if self.opened.is_some_and(|at| at.elapsed() >= AFTER) {
            self.opened = None;
            return crate::gpu_frames::adapter::vram_line("recording 30 s");
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_once_after_thirty_seconds() {
        let mut watch = VramWatch::default();
        assert!(watch.due().is_none(), "nothing before the encoder opens");
        watch.opened();
        assert!(watch.due().is_none(), "not yet");
        watch.opened = Some(Instant::now() - AFTER);
        let _ = watch.due(); // the line itself needs a known adapter
        assert!(watch.opened.is_none(), "one reading per open");
    }
}
