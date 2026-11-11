//! How an export turns decoded pictures into encoder frames: on the GPU ([`super::gpu`]) or
//! on the CPU (software decode and swscale), chosen once per encode attempt.

use ffmpeg_next as ff;

use super::video_encoder::{convert_into, open_decoder, scaler, SourceVideo};

pub(crate) enum Convert {
    Cpu(ff::software::scaling::Context),
    Gpu(Box<super::gpu::GpuStage>),
}

impl Convert {
    /// `decoded` as an encoder frame; the CPU path writes into `reuse` when it can.
    pub(crate) fn run(&mut self, decoded: &ff::frame::Video, reuse: Option<ff::frame::Video>) -> Result<ff::frame::Video, String> {
        match self {
            Convert::Cpu(scaler) => convert_into(scaler, decoded, reuse),
            Convert::Gpu(stage) => stage.convert(decoded),
        }
    }

    /// The D3D11 frames pool NVENC must be opened on (GPU path only).
    pub(crate) fn frames(&self) -> Option<*mut ff::ffi::AVBufferRef> {
        match self {
            Convert::Cpu(_) => None,
            Convert::Gpu(stage) => Some(stage.frames()),
        }
    }
}

/// The decoder and converter for a `width`x`height` export of `source`: the GPU path when
/// `gpu`, else the CPU path.
pub(crate) fn video_path(source: &SourceVideo, width: u32, height: u32, gpu: bool) -> Result<(ff::decoder::Video, Convert), String> {
    if gpu {
        let (decoder, stage) = super::gpu::open(source, (width, height))?;
        return Ok((decoder, Convert::Gpu(Box::new(stage))));
    }
    Ok((open_decoder(source.params.clone())?, Convert::Cpu(scaler(source, width, height)?)))
}

/// Runs `attempt` on the GPU path, and again on the CPU path when the GPU path fails (or
/// straight on the CPU when `gpu` is false). The GPU failure is logged, not returned.
pub(crate) fn with_fallback<T>(gpu: bool, mut attempt: impl FnMut(bool) -> Result<T, String>) -> Result<T, String> {
    if gpu {
        match attempt(true) {
            Ok(done) => return Ok(done),
            Err(why) => eprintln!("[recall] GPU export failed ({why}); exporting on the CPU"),
        }
    }
    attempt(false)
}
