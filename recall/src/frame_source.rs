//! Reading the newest stored GPU frame back after the privacy stop is possible (design-detect-
//! release P4): a screenshot and the HUD probe read `latest_gpu` on their own threads, and the
//! stop can land between a reader's `stopped()` check and its read. Both readers go through
//! [`read_admitted`], which re-checks the frame's number AFTER the read
//! (`stop_capture::frame_admitted`), so a frame stored after the stop is never used. The
//! source is a seam: production reads `latest_gpu`, tests hand in a frame numbered as they like.

/// Where a reader's frame comes from.
pub(crate) trait FrameSource<T> {
    /// Read the newest stored frame: its number (`stop_capture::next_frame_seq`, taken under the
    /// slot's lock) and the read's result. `None`: no frame stored yet.
    fn read_latest(&mut self) -> Option<(u64, Result<T, String>)>;
}

/// What a reader got.
#[derive(Debug, PartialEq)]
pub(crate) enum FrameRead<T> {
    Frame(T),
    /// The frame was stored after `stop_capture`: refused.
    Stopped,
    Failed(String),
    /// No frame stored yet.
    Empty,
}

/// Read through `src`, refusing a frame stored after the stop (checked after the read).
pub(crate) fn read_admitted<T>(src: &mut dyn FrameSource<T>) -> FrameRead<T> {
    let Some((seq, read)) = src.read_latest() else { return FrameRead::Empty };
    if !crate::stop_capture::frame_admitted(seq) {
        return FrameRead::Stopped;
    }
    match read {
        Ok(frame) => FrameRead::Frame(frame),
        Err(e) => FrameRead::Failed(e),
    }
}

/// `latest_gpu` with a read: the production source of both readers.
#[cfg(feature = "encode")]
pub(crate) struct LatestGpu<'a, R> {
    pub shared: &'a crate::capture::CaptureShared,
    /// Reads one cloned frame (its content rect: a windowed game's picture).
    pub read: R,
}

#[cfg(feature = "encode")]
impl<T, R> FrameSource<T> for LatestGpu<'_, R>
where
    R: FnMut(&crate::gpu_frames::HwFrame, Option<crate::framing::Rect>) -> Result<T, String>,
{
    fn read_latest(&mut self) -> Option<(u64, Result<T, String>)> {
        let (frame, content, seq) = {
            let slot = self.shared.latest_gpu.lock().ok()?;
            let seq = self.shared.latest_gpu_seq.load(std::sync::atomic::Ordering::Relaxed);
            let (frame, content) = slot.as_ref()?;
            (frame.try_clone(), *content, seq)
        }; // the lock is not held across the read
        let Some(frame) = frame else { return Some((seq, Err("frame clone failed".into()))) };
        Some((seq, (self.read)(&frame, content)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stop_capture::{next_frame_seq, request, reset_for_test};

    /// A source whose read the stop lands in (the reader's own `stopped()` check passed).
    pub(crate) struct StopDuringRead {
        /// The capture thread stored a new frame after the stop, before the read.
        pub stored_after: bool,
    }

    impl FrameSource<(u32, u32, Vec<u8>)> for StopDuringRead {
        fn read_latest(&mut self) -> Option<(u64, Result<(u32, u32, Vec<u8>), String>)> {
            let before = next_frame_seq();
            request();
            let seq = if self.stored_after { next_frame_seq() } else { before };
            Some((seq, Ok((1, 1, vec![9; 4]))))
        }
    }

    #[test]
    fn a_frame_stored_before_the_stop_is_read_one_stored_after_it_is_refused() {
        reset_for_test();
        assert_eq!(read_admitted(&mut StopDuringRead { stored_after: false }), FrameRead::Frame((1, 1, vec![9; 4])));
        reset_for_test();
        assert_eq!(read_admitted(&mut StopDuringRead { stored_after: true }), FrameRead::Stopped);
        reset_for_test();
    }

    /// The screenshot's GPU path end to end through the seam: the frame read was stored after
    /// the stop, so the shot fails ("capture stopped") and no PNG is written.
    #[test]
    fn a_screenshot_of_a_frame_stored_after_the_stop_is_not_saved() {
        reset_for_test();
        crate::protocol::test_emits::install();
        let out = std::env::temp_dir().join(format!("recall-shot-seam-{}", uuid::Uuid::new_v4().simple()));
        let shots = crate::screenshot::spawn(out.to_string_lossy().into_owned(), true);
        crate::app_tick::shoot(&shots, &mut StopDuringRead { stored_after: true }, "G");
        reset_for_test();
        let failed = crate::protocol::test_emits::of("screenshot_failed");
        assert!(failed.len() == 1 && failed[0].contains("capture stopped"), "{failed:?}");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!out.exists(), "nothing written");
    }

    /// The HUD probe's grab through the seam: never analyses a frame stored after the stop.
    #[test]
    fn the_hud_probe_refuses_a_frame_stored_after_the_stop() {
        struct Gray(StopDuringRead);
        impl FrameSource<crate::hud_probe::Gray> for Gray {
            fn read_latest(&mut self) -> Option<(u64, Result<crate::hud_probe::Gray, String>)> {
                let (seq, _) = self.0.read_latest()?;
                Some((seq, Ok(crate::hud_probe::Gray { w: 1, h: 1, px: vec![0.0] })))
            }
        }
        reset_for_test();
        assert_eq!(crate::hud_probe::grab_from(&mut Gray(StopDuringRead { stored_after: true })).err().as_deref(), Some("capture stopped"));
        reset_for_test();
        assert!(crate::hud_probe::grab_from(&mut Gray(StopDuringRead { stored_after: false })).is_ok(), "a frame from before the stop");
        reset_for_test();
    }
}
