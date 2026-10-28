//! Routing for the zero-copy path: whether it can run, and the capture thread's sink
//! that hands GPU frames straight to the encoder queue (main.rs installs it before capture
//! starts and closes it at quit).

#[cfg(feature = "encode")]
use std::sync::atomic::Ordering::Relaxed;
#[cfg(feature = "encode")]
use std::sync::{Arc, Mutex};

#[cfg(feature = "encode")]
use crate::capture::CaptureShared;
#[cfg(feature = "encode")]
use crate::{capture, encoder, gpu_frames, protocol};

/// The zero-copy path needs the encoder (and is skipped by the fp16 dump harness).
pub(crate) fn gpu_path_possible() -> bool {
    cfg!(feature = "encode") && std::env::var_os("RECALL_DUMP").is_none() && std::env::var_os("RECALL_DUMP_EVERY").is_none()
}

/// The capture thread's handle on the encoder queue; `None` once closed at quit.
#[cfg(feature = "encode")]
pub(crate) type GpuFramesTx = Arc<Mutex<Option<std::sync::mpsc::SyncSender<encoder::PacedFrame>>>>;

/// Route GPU frames from the capture thread straight into the encoder's queue. A full
/// queue drops the frame (never blocks capture); a gone encoder is reported like the CPU
/// path reports it.
#[cfg(feature = "encode")]
pub(crate) fn install_gpu_sink(shared: &Arc<CaptureShared>, handle: &encoder::EncoderHandle, json: bool) -> GpuFramesTx {
    let tx: GpuFramesTx = Arc::new(Mutex::new(Some(handle.frames.clone())));
    let frames = tx.clone();
    let stats = handle.stats.clone();
    let send = move |pts_us: u64, gpu: gpu_frames::HwFrame| {
        let (width, height) = (gpu.width(), gpu.height());
        let frame = encoder::PacedFrame { idx: pts_us, width, height, rgba: Vec::new(), gpu: Some(gpu) };
        let Ok(guard) = frames.lock() else { return false };
        let Some(frames) = guard.as_ref() else { return false };
        match frames.try_send(frame) {
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                stats.backend.store(3, Relaxed);
                if let Ok(mut error) = stats.error.lock() {
                    *error = Some("NVENC recording thread stopped; restart Recall".into());
                }
                false
            }
            _ => true,
        }
    };
    let log = move |message: String| {
        if json {
            protocol::emit(&protocol::Outbound::EncoderLog { message });
        } else {
            eprintln!("[recall] {message}");
        }
    };
    let _ = shared.gpu_sink.set(capture::GpuSink { send: Box::new(send), log: Box::new(log) });
    tx
}
