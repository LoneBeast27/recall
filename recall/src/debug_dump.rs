//! One-shot debug dump for verification without a camera in the loop.
//!
//! Decodes a captured `Rgba16F` (scRGB-linear) frame on the CPU, applies the
//! same passthrough/tonemap math as `tonemap.wgsl`, sRGB-encodes it, and writes
//! a faithful SDR PNG. Also reports per-channel linear stats and the fraction of
//! pixels above scRGB 1.0 — a direct "is this source HDR?" indicator.
//!
//! Gated behind the `RECALL_DUMP` env var; never runs in normal operation.

use std::fs;
use std::io;
use std::path::Path;

/// Decode an IEEE 754 half (f16) bit pattern to f32.
fn half_to_f32(h: u16) -> f32 {
    let sign = (h >> 15) & 1;
    let exp = (h >> 10) & 0x1f;
    let mant = h & 0x3ff;
    let val = if exp == 0 {
        (mant as f32) * 2f32.powi(-24) // subnormal
    } else if exp == 0x1f {
        if mant == 0 {
            f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        (1.0 + (mant as f32) / 1024.0) * 2f32.powi(exp as i32 - 15)
    };
    if sign == 1 {
        -val
    } else {
        val
    }
}

/// Linear -> sRGB gamma encode (matches the hardware sRGB swapchain store).
fn srgb_encode(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// Mirror of `tonemap_channel` in tonemap.wgsl: identity below the knee, soft
/// highlight rolloff toward 1.0 above it.
fn tonemap_channel(x: f32, white: f32) -> f32 {
    const KNEE: f32 = 0.8;
    if x <= KNEE {
        x
    } else {
        let t = (x - KNEE) / (white - KNEE).max(1e-3);
        KNEE + (1.0 - KNEE) * (1.0 - (-t).exp())
    }
}

/// Decode + tonemap one frame, write a PNG, and return a human-readable stats line.
/// `bytes` is tightly packed f16 RGBA (8 bytes/pixel), `mode` 0=passthrough 1=tonemap.
pub fn dump(
    width: u32,
    height: u32,
    bytes: &[u8],
    mode: u32,
    sdr_white: f32,
    hdr_white: f32,
    path: &str,
) -> io::Result<String> {
    let px = (width as usize) * (height as usize);
    if bytes.len() < px * 8 {
        return Err(io::Error::other("frame buffer smaller than width*height*8"));
    }

    let mut out = vec![0u8; px * 4];
    let (mut min_c, mut max_c, mut sum_c) = ([f32::MAX; 3], [f32::MIN; 3], [0f64; 3]);
    let mut over_one: u64 = 0;
    let white = hdr_white.max(1.0);
    let sdr = sdr_white.max(1e-4);

    for i in 0..px {
        let o = i * 8;
        let lin = [
            half_to_f32(u16::from_le_bytes([bytes[o], bytes[o + 1]])),
            half_to_f32(u16::from_le_bytes([bytes[o + 2], bytes[o + 3]])),
            half_to_f32(u16::from_le_bytes([bytes[o + 4], bytes[o + 5]])),
        ];
        let mut over = false;
        for c in 0..3 {
            let v = lin[c];
            min_c[c] = min_c[c].min(v);
            max_c[c] = max_c[c].max(v);
            sum_c[c] += v as f64;
            if v > 1.0 {
                over = true;
            }
        }
        if over {
            over_one += 1;
        }

        for c in 0..3 {
            let mapped = if mode == 0 {
                lin[c].clamp(0.0, 1.0)
            } else {
                tonemap_channel(lin[c].max(0.0) / sdr, white)
            };
            out[i * 4 + c] = (srgb_encode(mapped) * 255.0 + 0.5) as u8;
        }
        out[i * 4 + 3] = 255;
    }

    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).ok();
    }
    let file = fs::File::create(path)?;
    let writer = io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png_writer = encoder.write_header().map_err(io::Error::other)?;
    png_writer
        .write_image_data(&out)
        .map_err(io::Error::other)?;

    let n = px as f64;
    Ok(format!(
        "dump {w}x{h} -> {path}\n  linear  min[{:.3} {:.3} {:.3}]  max[{:.3} {:.3} {:.3}]  mean[{:.3} {:.3} {:.3}]\n  pixels >1.0 (HDR-range): {:.2}%  =>  source looks {hdr}\n  mode={mode} ({modename}), sdr_white={sdr_white}, hdr_white={hdr_white}",
        min_c[0], min_c[1], min_c[2],
        max_c[0], max_c[1], max_c[2],
        sum_c[0] / n, sum_c[1] / n, sum_c[2] / n,
        over_one as f64 / n * 100.0,
        w = width,
        h = height,
        path = path,
        hdr = if over_one as f64 / n > 0.001 { "HDR" } else { "SDR-range" },
        mode = mode,
        modename = if mode == 0 { "passthrough" } else { "tonemap" },
        sdr_white = sdr_white,
        hdr_white = hdr_white,
    ))
}
