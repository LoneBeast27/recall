//! How an export turns decoded pictures into encoder frames: on the GPU ([`super::gpu`]) or
//! on the CPU (software decode and swscale), and which of the two an export uses
//! ([`GpuChoice`]).

use std::cell::{Cell, RefCell};

use ffmpeg_next as ff;

use super::gpu::{self, Adapter};
use super::video_encoder::{convert_into, open_decoder, scaler, SourceVideo};

/// Starts the text of an error that belongs to the GPU path itself (its decoder, stage, or
/// the encoder refusing its frames): only those send the export to the next adapter and then
/// the CPU path. Every other error (bad input, a full disk, a mux error) ends the export.
pub(crate) const GPU_ERROR: &str = "GPU path: ";

pub(crate) fn gpu_err(error: impl std::fmt::Display) -> String {
    format!("{GPU_ERROR}{error}")
}

pub(crate) enum Convert {
    Cpu(ff::software::scaling::Context),
    Gpu(Box<gpu::GpuStage>),
}

impl Convert {
    /// `decoded` as an encoder frame; the CPU path writes into `reuse` when it can.
    pub(crate) fn run(&mut self, decoded: &ff::frame::Video, reuse: Option<ff::frame::Video>) -> Result<ff::frame::Video, String> {
        match self {
            Convert::Cpu(scaler) => convert_into(scaler, decoded, reuse),
            Convert::Gpu(stage) => stage.convert(decoded).map_err(gpu_err),
        }
    }

    /// The D3D11 frames pool the encoder must be opened on (GPU path only).
    pub(crate) fn frames(&self) -> Option<*mut ff::ffi::AVBufferRef> {
        match self {
            Convert::Cpu(_) => None,
            Convert::Gpu(stage) => Some(stage.frames()),
        }
    }

    /// An error opening the encoder: the GPU path's when it was opened on its frames.
    pub(crate) fn encoder_error(&self, error: String) -> String {
        if self.frames().is_some() { gpu_err(error) } else { error }
    }
}

/// The decoder and converter for a `width`x`height` export of `source`: on `adapter`'s GPU,
/// or the CPU path with none.
pub(crate) fn video_path(source: &SourceVideo, width: u32, height: u32, adapter: Option<&Adapter>) -> Result<(ff::decoder::Video, Convert), String> {
    if let Some(adapter) = adapter {
        let (decoder, stage) = gpu::open(source, (width, height), adapter).map_err(|e| gpu_err(format!("{}: {e}", adapter.name)))?;
        return Ok((decoder, Convert::Gpu(Box::new(stage))));
    }
    Ok((open_decoder(source.params.clone())?, Convert::Cpu(scaler(source, width, height)?)))
}

/// Which path one export runs on. The GPU path is tried on each hardware adapter in turn;
/// an adapter that failed with a GPU error is not tried again by a later attempt of the same
/// export (the size-limit loop), and with none left the CPU path runs.
pub(crate) struct GpuChoice {
    adapters: RefCell<Vec<Adapter>>,
    /// Why the GPU path was not used (or the last GPU error), for the report.
    why_not: RefCell<Option<String>>,
    used_gpu: Cell<bool>,
    /// A GPU attempt ran and failed (so a CPU error is reported with it).
    gpu_failed: Cell<bool>,
}

impl GpuChoice {
    /// `allowed`: the export may use the GPU path (`Err` = why not).
    pub(crate) fn new(allowed: Result<(), String>) -> Self {
        let (adapters, why_not) = match allowed.and_then(|()| gpu::adapters()) {
            Ok(list) if list.is_empty() => (Vec::new(), Some("no hardware GPU adapter".to_owned())),
            Ok(list) => (list, None),
            Err(why) => (Vec::new(), Some(why)),
        };
        Self { adapters: RefCell::new(adapters), why_not: RefCell::new(why_not), used_gpu: Cell::new(false), gpu_failed: Cell::new(false) }
    }

    /// For an export through `nvenc` (false: a test encoder, which has no GPU path).
    pub(crate) fn for_export(nvenc: bool) -> Self {
        Self::new(if !nvenc {
            Err("the encoder is not NVENC".to_owned())
        } else if !gpu::wanted() {
            Err("turned off (RECALL_EXPORT_GPU=0)".to_owned())
        } else {
            Ok(())
        })
    }

    /// Runs `attempt` on the first adapter left; a GPU error drops that adapter and tries the
    /// next, then the CPU path (`None`). A CPU failure after a GPU one carries both texts.
    pub(crate) fn run<T>(&self, mut attempt: impl FnMut(Option<&Adapter>) -> Result<T, String>) -> Result<T, String> {
        loop {
            let Some(adapter) = self.adapters.borrow().first().cloned() else { break };
            match attempt(Some(&adapter)) {
                Ok(done) => {
                    self.used_gpu.set(true);
                    return Ok(done);
                }
                Err(why) if why.starts_with(GPU_ERROR) => {
                    eprintln!("[recall] {why}; trying the next path");
                    self.adapters.borrow_mut().remove(0);
                    // The first adapter's error is the telling one (the best GPU's).
                    if !self.gpu_failed.replace(true) {
                        *self.why_not.borrow_mut() = Some(why[GPU_ERROR.len()..].to_owned());
                    }
                }
                Err(why) => return Err(why),
            }
        }
        self.used_gpu.set(false);
        attempt(None).map_err(|cpu| match self.why_not.borrow().as_deref() {
            Some(gpu) if self.gpu_failed.get() => format!("{cpu} (the GPU path failed first: {gpu})"),
            _ => cpu,
        })
    }

    /// For the export report: whether the last attempt ran on the GPU, and if not, why.
    pub(crate) fn report(&self) -> (bool, Option<String>) {
        let gpu = self.used_gpu.get();
        (gpu, if gpu { None } else { self.why_not.borrow().clone() })
    }
}
