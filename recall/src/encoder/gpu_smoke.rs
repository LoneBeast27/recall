//! `--gpu-smoke <dir>`: the zero-copy recording path without WGC. Synthetic 2560x1440 fp16
//! frames (two alternating patterns) go through the D3D11 tonemap pipeline into NVENC on
//! the same device, like the capture thread does, then the saver writes a clip (with the
//! readback thumbnail). Prints JSON with the CPU time this thread spent per frame.
//! GPU work: the caller owns resource admission.

use std::time::{Duration, Instant};

use ffmpeg_next as ff;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC};
use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};

use super::{gpu_pool_of, open_encoder, saver, Config, EncStats, PacedFrame, SaveRequest};
use crate::framing::Layout;
use crate::gpu_frames::{hardware_device, read_rgba, target_size, Pipeline};

const W: u32 = 2560;
const H: u32 = 1440;
const FRAMES: u64 = 240;

/// A `W`x`H` Rgba16F texture: a grey ramp with a bar whose place depends on `phase`.
fn source(dev: &ID3D11Device, phase: u32) -> Result<ID3D11Texture2D, String> {
    let one = 0x3c00u16; // 1.0 in f16
    let mut px = vec![0u16; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let bar = (x / 64 + phase) % 8 == 0;
            // Ramp in f16 steps (0..1 over the width): exponent-mantissa from a scaled int.
            let v = (x as f32 / W as f32).max(1e-3);
            let bits = half_bits(v);
            px[i..i + 4].copy_from_slice(&[if bar { one } else { bits }, bits, if bar { 0 } else { bits }, one]);
        }
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: W,
        Height: H,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr().cast(), SysMemPitch: W * 8, SysMemSlicePitch: 0 };
    let mut tex = None;
    unsafe { dev.CreateTexture2D(&desc, Some(&init), Some(&mut tex)) }.map_err(|e| format!("source texture: {e}"))?;
    tex.ok_or_else(|| "source texture missing".into())
}

fn half_bits(v: f32) -> u16 {
    let bits = v.to_bits();
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    if exp <= 0 {
        return 0;
    }
    ((exp as u16) << 10) | ((bits & 0x7f_ffff) >> 13) as u16
}

/// User + kernel CPU time of the calling thread.
fn thread_cpu() -> Duration {
    use windows::Win32::Foundation::FILETIME;
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        let _ = GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u);
    }
    let t = |f: windows::Win32::Foundation::FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) * 100;
    Duration::from_nanos(t(k) + t(u))
}

pub fn gpu_smoke(out_dir: &std::path::Path) -> Result<String, String> {
    ff::init().map_err(|e| format!("ffmpeg init: {e}"))?;
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let cfg = Config { fps: 60, buffer_secs: 10, ..Config::from_env(false) };
    let (dev, ctx) = hardware_device().ok_or("no hardware D3D11 device")?;
    let sources = [source(&dev, 0)?, source(&dev, 4)?];
    let mut pipe = Pipeline::new(&dev, &ctx)?;
    let stats = EncStats::default();
    let mut enc: Option<super::Enc> = None;
    let started = Instant::now();
    let cpu0 = thread_cpu();
    let (mut render_max, mut render_sum, mut reopens) = (Duration::ZERO, Duration::ZERO, 0u32);
    for i in 0..FRAMES {
        let target = target_size(pipe.target(), (W, H));
        let t = Instant::now();
        let gpu = pipe.render(&sources[(i / 8 % 2) as usize], &Layout::native((W, H), target), 0)?;
        let took = t.elapsed();
        render_sum += took;
        render_max = render_max.max(took);
        let frame = PacedFrame { idx: i * 1_000_000 / 60, width: target.0, height: target.1, rgba: Vec::new(), gpu: Some(gpu), content: None };
        // The live loop's reopen rule: a frame from another pool reopens NVENC.
        if enc.as_ref().is_some_and(|e| e.gpu_pool != gpu_pool_of(&frame)) {
            reopens += 1;
        }
        if enc.is_none() {
            enc = Some(open_encoder(&cfg, cfg.codec, target.0, target.1, frame.gpu.as_ref())?.0);
        }
        enc.as_mut().expect("opened above").encode(frame, &stats)?;
        // Real time, like capture at 60 fps.
        let due = started + Duration::from_micros((i + 1) * 1_000_000 / 60);
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
    }
    let cpu = thread_cpu() - cpu0;
    if reopens > 0 {
        return Err(format!("{reopens} frames looked like a new frames pool (NVENC would reopen)"));
    }
    let mut enc = enc.ok_or("no frames encoded")?;
    enc.encoder.send_eof().map_err(|e| format!("NVENC flush: {e}"))?;
    enc.drain(&stats)?;
    let thumb = enc.raw_last().ok_or("thumbnail readback failed")?;
    let probe = read_rgba(enc.last_gpu.as_ref().ok_or("no last frame")?)?;
    let path = out_dir.join(format!("gpu-smoke-{}.mp4", uuid::Uuid::new_v4()));
    let path_text = path.to_string_lossy().into_owned();
    let now = Instant::now();
    let req = SaveRequest { path: path_text.clone(), seconds: 4, end_us: None, at_last_frame: false, requested: now };
    saver::write_now(&enc.build_job(req, &[], now)?)?;
    // The same last frame through the CPU path's encoder input (RGB0 from the readback),
    // as a quiet-tail still: decoding both shows whether D3D11 BGRA input colours match.
    let still = out_dir.join(format!("gpu-smoke-rgb0-{}.mp4", uuid::Uuid::new_v4()));
    let still_text = still.to_string_lossy().into_owned();
    let req = SaveRequest { path: still_text.clone(), seconds: 1, end_us: Some(300_000_000), at_last_frame: false, requested: now };
    saver::write_now(&enc.build_job(req, &[], now)?)?;
    let out = serde_json::json!({
        "clip": path_text,
        "rgb0_still": still_text,
        "codec": enc.backend,
        "size": [enc.width, enc.height],
        "frames": FRAMES,
        "encoded": stats.encoded_frames.load(std::sync::atomic::Ordering::Relaxed),
        "thread_cpu_ms_per_frame": cpu.as_secs_f64() * 1000.0 / FRAMES as f64,
        "render_wall_ms_mean": render_sum.as_secs_f64() * 1000.0 / FRAMES as f64,
        "render_wall_ms_max": render_max.as_secs_f64() * 1000.0,
        "pool_reopens": reopens,
        "wall_s": started.elapsed().as_secs_f64(),
        "thumb_size": [thumb.0, thumb.1],
        // Pixel (10, 10) of the last frame: ramp ~0.004 linear -> sRGB ~12.
        "probe_rgba_10_10": &probe.2[((10 * probe.0 + 10) * 4) as usize..][..4],
    });
    Ok(out.to_string())
}
