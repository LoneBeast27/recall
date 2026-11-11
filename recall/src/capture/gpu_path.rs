//! The capture thread's zero-copy half: each captured frame (WGC or Desktop Duplication) is tonemapped on the capture device
//! into an FFmpeg D3D11 frame (`gpu_frames::Pipeline`) and handed to the encoder queue.

use std::sync::atomic::Ordering::Relaxed;

use super::{Handler, Source, PATH_GPU};
use crate::framing::{Layout, Rect};

/// Consecutive GPU-path failures after which capture falls back to the CPU path.
const GPU_FAILURES_BEFORE_FALLBACK: u32 = 30;

/// The capture thread's zero-copy pipeline, built on the first frame (it needs the
/// capture device).
pub(super) enum GpuState {
    Untried,
    /// `small`: in a run of frames too small to encode (logged once per run).
    Ready { pipe: Box<crate::gpu_frames::Pipeline>, failures: u32, small: bool },
    Off,
}

impl Handler {
    /// Zero-copy path: tonemap this frame on the GPU and hand it to the encoder. `crop` is
    /// the window's client area and `canvas` the canvas the session records on, if any
    /// (`client_area::ClientArea::update`). False sends the frame down the CPU path (no
    /// encoder sink, the pipeline cannot start, or it kept failing).
    pub(super) fn gpu_frame(&mut self, src: &Source<'_>, pts_us: u64, crop: Option<Rect>, canvas: Option<(u32, u32)>) -> bool {
        let Some(sink) = self.shared.gpu_sink.get() else { return false };
        if matches!(self.gpu, GpuState::Off) && (sink.retry_gpu)() {
            (sink.log)("capture: trying the zero-copy GPU path again".into());
            self.gpu = GpuState::Untried;
        }
        if let GpuState::Untried = self.gpu {
            match crate::gpu_frames::adapter::remember(src.device) {
                Some(gpu) => (sink.log)(format!("capture: device on {}", gpu.describe())),
                None => (sink.log)("capture: device adapter unknown".into()),
            }
            self.gpu = match crate::gpu_frames::Pipeline::new(src.device, src.context) {
                Ok(pipe) => {
                    (sink.log)("capture: zero-copy GPU path (D3D11 tonemap -> NVENC)".into());
                    if let Some(line) = crate::gpu_frames::adapter::vram_line("capture device and pipeline") {
                        (sink.log)(line);
                    }
                    GpuState::Ready { pipe: Box::new(pipe), failures: 0, small: false }
                }
                Err(e) => {
                    (sink.log)(format!("capture: GPU path unavailable ({e}); using CPU copies"));
                    GpuState::Off
                }
            };
        }
        let GpuState::Ready { pipe, failures, small } = &mut self.gpu else { return false };
        let size = src.size;
        let Some(layout) = Layout::recorded(size, crop, canvas, pipe.target()) else {
            if !std::mem::replace(small, true) {
                (sink.log)(format!("capture: skipping {}x{} frames, too small to encode", size.0, size.1));
            }
            return true;
        };
        *small = false;
        let mode = self.shared.mode.load(Relaxed);
        if let Some(note) = pipe.source_note(src.texture) {
            (sink.log)(format!("capture: {note}"));
        }
        match pipe.render(src.texture, &layout, mode) {
            Ok(hw) => {
                if self.frame_count == 0 {
                    if let Some(line) = crate::gpu_frames::adapter::vram_line("first frame (capture pool, frame pool)") {
                        (sink.log)(line);
                    }
                }
                *failures = 0;
                self.frame_count += 1;
                self.shared.path.store(PATH_GPU, Relaxed);
                if let Ok(mut size) = self.shared.gpu_size.lock() {
                    *size = Some(layout.out);
                }
                // R2-detect-r10-p6-3: no frame is stored for a screenshot or the HUD probe once
                // `stop_capture` is set; a frame stored is numbered after this check.
                if !crate::stop_capture::stopped() {
                    if let (Some(copy), Ok(mut latest)) = (hw.try_clone(), self.shared.latest_gpu.lock()) {
                        *latest = Some((copy, layout.content()));
                        self.shared.latest_gpu_seq.store(crate::stop_capture::next_frame_seq(), Relaxed);
                    }
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
