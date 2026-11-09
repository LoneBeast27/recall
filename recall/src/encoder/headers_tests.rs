//! Hardware check: the parameter sets NVENC writes for D3D11 (zero-copy) input and for RGB0
//! (CPU copies) input at the same size and codec. Equal bytes mean a save can join the
//! replay buffer across a GPU <-> CPU switch (`replay.rs`). Prints the bytes; skips (and
//! says so) without NVENC.

use super::frame_fit::wobble_tests::{nvenc, Capture, BIG};
use super::mux::OwnedParams;
use super::settings::Codec;
use super::{try_open, Config};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// FNV-1a 64 of the bytes (a short fingerprint for RESULT.md).
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3))
}

/// (codec, size, D3D11 parameter sets, RGB0 parameter sets).
type Measured = (Codec, (u32, u32), Vec<u8>, Vec<u8>);

/// [`Measured`] for every codec that opens, at three sizes.
pub(super) fn measured() -> Vec<Measured> {
    let Some(mut capture) = Capture::new() else { return Vec::new() };
    ffmpeg_next::init().unwrap();
    let cfg = Config { fps: 60, buffer_secs: 5, bitrate: 2_000_000, ..Config::from_env(false) };
    let mut out = Vec::new();
    for size in [BIG, (1920, 1080), (2560, 1440)] {
        let frame = capture.render(size);
        out.extend(per_codec(&cfg, &frame));
    }
    out
}

fn per_codec(cfg: &Config, frame: &crate::gpu_frames::HwFrame) -> Vec<Measured> {
    let (w, h) = (frame.width(), frame.height());
    let mut out = Vec::new();
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        let (Ok(gpu), Ok(cpu)) = (try_open(cfg, codec, w, h, Some(frame.frames_ctx())), try_open(cfg, codec, w, h, None)) else {
            crate::hw_skip::skipped(&format!("{} does not open here on both inputs", codec.nvenc()));
            continue;
        };
        out.push((codec, (w, h), OwnedParams::from_encoder(&gpu).extradata(), OwnedParams::from_encoder(&cpu).extradata()));
    }
    out
}

#[test]
fn nvenc_parameter_sets_for_d3d11_and_rgb0_input() {
    let _one = nvenc();
    for (codec, (w, h), gpu, cpu) in measured() {
        let line = format!(
            "{} {w}x{h}: d3d11 {} bytes fnv {:016x} {} | rgb0 {} bytes fnv {:016x} {} | {}",
            codec.nvenc(),
            gpu.len(),
            fnv(&gpu),
            hex(&gpu),
            cpu.len(),
            fnv(&cpu),
            hex(&cpu),
            if gpu == cpu { "SAME" } else { "DIFFERENT" }
        );
        // Straight to stderr: the measurement is the point of this test.
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "HEADERS {line}");
        assert!(!gpu.is_empty() && !cpu.is_empty(), "{line}");
    }
}
