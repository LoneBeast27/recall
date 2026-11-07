//! Source video probing, the pixel converter/downscaler, and the video encoder
//! used by re-encoding exports.
//!
//! Production always encodes with NVENC (no CPU fallback): `h264_nvenc`, or
//! `hevc_nvenc` for a frame wider or taller than H.264 NVENC allows (see
//! [`nvenc_codec`]). The encoder is a parameter so CPU-only tests can drive the
//! identical pipeline with libx264; that variant only exists in test builds.

use ffmpeg_next as ff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoBackend {
    Nvenc,
    #[cfg(test)]
    X264,
}

/// NVENC's H.264 encoder takes frames up to 4096 px a side; HEVC goes to 8192.
const H264_NVENC_MAX: u32 = 4096;
const HEVC_NVENC_MAX: u32 = 8192;

/// The NVENC encoder for a `width` x `height` export: H.264 (plays everywhere) when it fits,
/// else HEVC (32:9 5120x1440 and 8K sources), else an error that says what to pick.
pub(crate) fn nvenc_codec(width: u32, height: u32) -> Result<&'static str, String> {
    let side = width.max(height);
    if side <= H264_NVENC_MAX {
        Ok("h264_nvenc")
    } else if side <= HEVC_NVENC_MAX {
        Ok("hevc_nvenc")
    } else {
        Err(format!(
            "this clip is {width}x{height}; NVIDIA encoders take at most {HEVC_NVENC_MAX} px a side, so pick 1080p (or 720p) under Resolution"
        ))
    }
}

impl VideoBackend {
    fn codec_name(self, width: u32, height: u32) -> Result<&'static str, String> {
        match self {
            Self::Nvenc => nvenc_codec(width, height),
            #[cfg(test)]
            Self::X264 => Ok("libx264"),
        }
    }

    fn options(self) -> ff::Dictionary<'static> {
        let mut options = ff::Dictionary::new();
        match self {
            Self::Nvenc => {
                // p4 over p5: a 22 s 1440p60 export took 6.0 s instead of 8.0 s
                // at the same size, SSIM against the source 0.92544 vs 0.92568.
                options.set("preset", "p4");
                options.set("tune", "hq");
                options.set("rc", "vbr");
                options.set("bf", "0");
            }
            #[cfg(test)]
            Self::X264 => {
                options.set("preset", "veryfast");
                options.set("bf", "0");
            }
        }
        options
    }

    fn unavailable(self, name: &str) -> String {
        match self {
            Self::Nvenc => {
                format!("NVIDIA NVENC encoder {name} is unavailable; no CPU fallback is permitted")
            }
            #[cfg(test)]
            Self::X264 => "libx264 is unavailable in this FFmpeg build".into(),
        }
    }
}

/// Average bitrate plus an optional VBV ceiling (`maxrate` over `bufsize`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RateControl {
    pub bitrate: usize,
    pub vbv: Option<Vbv>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Vbv {
    pub maxrate: usize,
    pub bufsize: usize,
}

impl RateControl {
    pub fn quality(bitrate: usize) -> Self {
        Self { bitrate, vbv: None }
    }
}

/// Quality-oriented default bitrate: 0.1 bit per pixel, 4–120 Mbps.
pub(crate) fn recommended_bitrate(width: u32, height: u32, fps: u32) -> usize {
    let pixels_per_second = width as u128 * height as u128 * fps as u128;
    let bits = (pixels_per_second / 10).clamp(4_000_000, 120_000_000);
    bits as usize
}

pub(crate) struct SourceVideo {
    pub index: usize,
    pub time_base: ff::Rational,
    pub params: ff::codec::Parameters,
    pub width: u32,
    pub height: u32,
    pub format: ff::format::Pixel,
    pub aspect: ff::Rational,
}

pub(crate) fn probe_video(ictx: &ff::format::context::Input) -> Result<SourceVideo, String> {
    let stream = ictx
        .streams()
        .best(ff::media::Type::Video)
        .ok_or_else(|| "input has no decodable video stream".to_string())?;
    let decoder = open_decoder(stream.parameters())?;
    if decoder.width() == 0 || decoder.height() == 0 {
        return Err("input video has invalid zero dimensions".into());
    }
    Ok(SourceVideo {
        index: stream.index(),
        time_base: stream.time_base(),
        params: stream.parameters(),
        width: decoder.width(),
        height: decoder.height(),
        format: decoder.format(),
        aspect: decoder.aspect_ratio(),
    })
}

/// Software decoder with frame threads. libavcodec defaults API users to one
/// thread, which held 1440p60 exports near 100 decoded frames a second; count
/// 0 lets it size the pool from the process affinity (CCD1 when pinned).
pub(crate) fn open_decoder(params: ff::codec::Parameters) -> Result<ff::decoder::Video, String> {
    let mut context = ff::codec::context::Context::from_parameters(params)
        .map_err(|e| format!("create video decoder: {e}"))?;
    context.set_threading(ff::codec::threading::Config {
        kind: ff::codec::threading::Type::Frame,
        count: 0,
        ..Default::default()
    });
    context
        .decoder()
        .video()
        .map_err(|e| format!("open video decoder: {e}"))
}

/// Output size for an optional target height: never upscales, keeps the aspect
/// ratio, and rounds both sides to even numbers (4:2:0 chroma needs them).
pub(crate) fn output_dims(width: u32, height: u32, target_height: Option<u32>) -> (u32, u32) {
    let even = |value: u32| (value & !1).max(2);
    match target_height {
        Some(target) if target < height => {
            let out_height = even(target);
            let out_width = (width as f64 * out_height as f64 / height as f64).round() as u32;
            (even(out_width + (out_width & 1)), out_height)
        }
        _ => (even(width), even(height)),
    }
}

/// Converts decoded frames to YUV 4:2:0 at the output size in one swscale pass.
/// Bicubic when downscaling (sharper than bilinear at similar cost); bilinear
/// for the same-size format conversion.
pub(crate) fn scaler(
    source: &SourceVideo,
    width: u32,
    height: u32,
) -> Result<ff::software::scaling::Context, String> {
    let flags = if width == source.width && height == source.height {
        ff::software::scaling::Flags::BILINEAR
    } else {
        ff::software::scaling::Flags::BICUBIC
    };
    ff::software::scaling::Context::get(
        source.format,
        source.width,
        source.height,
        ff::format::Pixel::YUV420P,
        width,
        height,
        flags,
    )
    .map_err(|e| format!("create video pixel converter: {e}"))
}

pub(crate) struct EncoderSpec {
    pub backend: VideoBackend,
    pub width: u32,
    pub height: u32,
    pub aspect: ff::Rational,
    pub fps: u32,
    pub rate: RateControl,
}

/// Open the video encoder (CFR `1/fps` timebase, one keyframe per second, no
/// B-frames, global headers for MP4).
pub(crate) fn open_encoder(spec: &EncoderSpec) -> Result<(ff::encoder::Video, ff::Codec), String> {
    let name = spec.backend.codec_name(spec.width, spec.height)?;
    let codec = ff::encoder::find_by_name(name).ok_or_else(|| spec.backend.unavailable(name))?;
    let mut context = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(|e| format!("create {name} context: {e}"))?;
    context.set_width(spec.width);
    context.set_height(spec.height);
    context.set_aspect_ratio(spec.aspect);
    context.set_format(ff::format::Pixel::YUV420P);
    context.set_time_base(ff::Rational(1, spec.fps as i32));
    context.set_frame_rate(Some(ff::Rational(spec.fps as i32, 1)));
    context.set_gop(spec.fps.max(1));
    context.set_max_b_frames(0);
    context.set_bit_rate(spec.rate.bitrate);
    context.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut options = spec.backend.options();
    if let Some(vbv) = spec.rate.vbv {
        options.set("maxrate", &vbv.maxrate.to_string());
        options.set("bufsize", &vbv.bufsize.to_string());
    }
    let encoder = context.open_with(options).map_err(|e| match spec.backend {
        VideoBackend::Nvenc => format!(
            "open NVIDIA NVENC encoder {name} at {}x{}: {e}; no CPU fallback is permitted (a smaller Resolution such as 1080p may work)",
            spec.width, spec.height
        ),
        #[cfg(test)]
        VideoBackend::X264 => format!("open libx264: {e}"),
    })?;
    Ok((encoder, codec))
}

#[cfg(test)]
mod tests {
    use super::{nvenc_codec, output_dims};

    #[test]
    fn frames_past_h264_nvenc_use_hevc_and_past_hevc_say_pick_1080p() {
        assert_eq!(nvenc_codec(3840, 2160), Ok("h264_nvenc"));
        assert_eq!(nvenc_codec(4096, 2160), Ok("h264_nvenc"));
        assert_eq!(nvenc_codec(5120, 1440), Ok("hevc_nvenc"), "32:9 at Original");
        assert_eq!(nvenc_codec(7680, 4320), Ok("hevc_nvenc"));
        assert_eq!(nvenc_codec(1440, 5120), Ok("hevc_nvenc"), "portrait counts the long side");
        let error = nvenc_codec(10240, 1440).unwrap_err();
        assert!(error.contains("pick 1080p"), "{error}");
    }

    #[test]
    fn downscale_keeps_aspect_with_even_sides_and_never_upscales() {
        assert_eq!(output_dims(2560, 1440, Some(1080)), (1920, 1080));
        assert_eq!(output_dims(1920, 1080, Some(720)), (1280, 720));
        assert_eq!(output_dims(3440, 1440, Some(1080)), (2580, 1080));
        assert_eq!(output_dims(322, 240, Some(135)), (180, 134));
        assert_eq!(output_dims(1920, 1080, Some(1080)), (1920, 1080));
        assert_eq!(output_dims(1280, 720, Some(1080)), (1280, 720));
        assert_eq!(output_dims(1920, 1080, None), (1920, 1080));
    }

    /// `cargo test bench_swscale -- --ignored --nocapture`: per-frame cost of the
    /// decoded-frame conversion at the same size vs. downscaled (the measured
    /// basis for scaling in swscale rather than adding a GPU scaler stage).
    #[test]
    #[ignore]
    fn bench_swscale_downscale() {
        use super::*;
        ff::init().unwrap();
        let (width, height) = (2560u32, 1440u32);
        let mut source = ff::frame::Video::new(ff::format::Pixel::YUV420P, width, height);
        for plane in 0..3 {
            for (i, byte) in source.data_mut(plane).iter_mut().enumerate() {
                *byte = (i * 31 % 251) as u8;
            }
        }
        let probe = SourceVideo {
            index: 0,
            time_base: ff::Rational(1, 1_000_000),
            params: ff::codec::Parameters::new(),
            width,
            height,
            format: ff::format::Pixel::YUV420P,
            aspect: ff::Rational(1, 1),
        };
        for target in [None, Some(1080), Some(720)] {
            let (w, h) = output_dims(width, height, target);
            let mut context = scaler(&probe, w, h).unwrap();
            let mut out = ff::frame::Video::new(ff::format::Pixel::YUV420P, w, h);
            let rounds = 120;
            let started = std::time::Instant::now();
            for _ in 0..rounds {
                context.run(&source, &mut out).unwrap();
            }
            let per_frame = started.elapsed().as_secs_f64() * 1000.0 / rounds as f64;
            eprintln!("swscale 2560x1440 -> {w}x{h}: {per_frame:.2} ms/frame");
        }
    }
}
