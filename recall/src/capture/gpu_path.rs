//! The capture thread's zero-copy half: each WGC frame is tonemapped on the capture device
//! into an FFmpeg D3D11 frame (`gpu_frames::Pipeline`) and handed to the encoder queue.

use std::sync::atomic::Ordering::Relaxed;

use windows_capture::frame::Frame;

use super::{Handler, PATH_GPU};
use crate::framing::{Layout, Rect};

/// Consecutive GPU-path failures after which capture falls back to the CPU path.
const GPU_FAILURES_BEFORE_FALLBACK: u32 = 30;

/// The capture thread's zero-copy pipeline, built on the first frame (it needs the
/// capture device).
pub(super) enum GpuState {
    Untried,
    Ready { pipe: crate::gpu_frames::Pipeline, failures: u32 },
    Off,
}

impl Handler {
    /// Zero-copy path: tonemap this frame on the GPU and hand it to the encoder. `crop` is
    /// the window's client area and `canvas` whether the session records on the canvas
    /// (`client_area::ClientArea::update`). False sends the frame down the CPU path (no
    /// encoder sink, the pipeline cannot start, or it kept failing).
    pub(super) fn gpu_frame(&mut self, frame: &mut Frame, pts_us: u64, crop: Option<Rect>, canvas: bool) -> bool {
        let Some(sink) = self.shared.gpu_sink.get() else { return false };
        if let GpuState::Untried = self.gpu {
            self.gpu = match crate::gpu_frames::Pipeline::new(frame.device(), frame.device_context()) {
                Ok(pipe) => {
                    (sink.log)("capture: zero-copy GPU path (D3D11 tonemap -> NVENC)".into());
                    GpuState::Ready { pipe, failures: 0 }
                }
                Err(e) => {
                    (sink.log)(format!("capture: GPU path unavailable ({e}); using CPU copies"));
                    GpuState::Off
                }
            };
        }
        let GpuState::Ready { pipe, failures } = &mut self.gpu else { return false };
        let size = (frame.width(), frame.height());
        let Some(layout) = Layout::recorded(size, crop, canvas, pipe.target()) else { return true };
        let mode = self.shared.mode.load(Relaxed);
        match pipe.render(frame.as_raw_texture(), &layout, mode) {
            Ok(hw) => {
                *failures = 0;
                self.frame_count += 1;
                self.shared.path.store(PATH_GPU, Relaxed);
                if let Ok(mut size) = self.shared.gpu_size.lock() {
                    *size = Some(layout.out);
                }
                if let (Some(copy), Ok(mut latest)) = (hw.try_clone(), self.shared.latest_gpu.lock()) {
                    *latest = Some((copy, layout.content()));
                }
                if !(sink.send)(pts_us, hw, layout.content()) {
                    (sink.log)("capture: encoder takes no GPU frames; using CPU copies".into());
                    self.gpu_off();
                }
                true
            }
            Err(e) => {
                *failures += 1;
                if *failures == 1 || *failures >= GPU_FAILURES_BEFORE_FALLBACK {
                    (sink.log)(format!("capture: GPU frame failed ({e}), failure {}", *failures));
                }
                if *failures < GPU_FAILURES_BEFORE_FALLBACK {
                    return true; // skip this frame; the next one retries the GPU path
                }
                (sink.log)("capture: falling back to CPU copies".into());
                self.gpu_off();
                false
            }
        }
    }

    /// Leave the GPU path for good (frames take the CPU path from now on).
    fn gpu_off(&mut self) {
        self.gpu = GpuState::Off;
        if let Ok(mut latest) = self.shared.latest_gpu.lock() {
            *latest = None;
        }
    }
}
