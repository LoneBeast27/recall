//! Size policy, plus the shader checked on the real GPU against a CPU reference of the same
//! math (tonemap.wgsl's passthrough / knee tonemap + sRGB encode), through the same FFmpeg
//! frames pool and readback the recorder uses.

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC};

use super::{hardware_device, read_rgba, target_size, Pipeline};

#[test]
fn target_keeps_size_across_border_jitter_and_reopens_on_real_change() {
    assert_eq!(target_size(None, (2561, 1441)), (2560, 1440), "even crop for NVENC");
    assert_eq!(target_size(Some((2560, 1440)), (2566, 1434)), (2560, 1440), "6 px jitter absorbed");
    assert_eq!(target_size(Some((2560, 1440)), (2560, 1449)), (2560, 1448), "9 px is a new size");
    assert_eq!(target_size(Some((2560, 1440)), (1920, 1080)), (1920, 1080));
    assert_eq!(target_size(None, (1, 1)), (2, 2));
}

fn f16(v: f32) -> u16 {
    // Normal-range f32 -> f16 (round to nearest), enough for test patches.
    if v == 0.0 {
        return 0;
    }
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mant = bits & 0x7f_ffff;
    let rounded = (mant + 0x1000) >> 13;
    let (exp, mant) = if rounded == 0x400 { (exp + 1, 0) } else { (exp, rounded) };
    sign | ((exp as u16) << 10) | mant as u16
}

fn f16_to_f32(h: u16) -> f32 {
    let exp = ((h >> 10) & 0x1f) as i32;
    let mant = (h & 0x3ff) as f32;
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    if exp == 0 {
        return sign * mant * 2f32.powi(-24);
    }
    sign * (1.0 + mant / 1024.0) * 2f32.powi(exp - 15)
}

/// tonemap.wgsl + the sRGB encode of its *-Srgb target, as bytes.
fn reference(c: [f32; 3], mode: u32) -> [u8; 3] {
    let tm = |x: f32, w: f32| {
        let knee = 0.8;
        if x <= knee {
            return x;
        }
        let t = (x - knee) / (w - knee).max(1e-3);
        knee + (1.0 - knee) * (1.0 - (-t).exp())
    };
    let enc = |c: f32| if c <= 0.0031308 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    c.map(|x| {
        let v = if mode == 0 { x.clamp(0.0, 1.0) } else { tm(x.max(0.0), 6.0).clamp(0.0, 1.0) };
        (enc(v) * 255.0).round() as u8
    })
}

/// Patches in scRGB: SDR values, the 0.8 knee, HDR highlights and an out-of-gamut negative.
const PATCHES: [[f32; 3]; 8] = [
    [0.0, 0.0, 0.0],
    [1.0, 1.0, 1.0],
    [0.2158, 0.2158, 0.2158], // sRGB 128
    [1.0, 0.0, 0.0],
    [0.05, 0.5, 0.9],
    [2.0, 1.2, 0.85],
    [12.5, 6.0, 3.0],
    [-0.1, 0.3, 1.5],
];

#[test]
fn gpu_output_matches_the_wgsl_reference_and_pads_black() {
    let Some((dev, ctx)) = hardware_device() else {
        eprintln!("no hardware D3D11 device; skipping");
        return;
    };
    // 8 patches of 4x4 texels in one row: a 32x4 source, rendered into a 36x6 target.
    let (sw, sh) = (32u32, 4u32);
    let mut texels = vec![0u16; (sw * sh * 4) as usize];
    for y in 0..sh {
        for x in 0..sw {
            let p = PATCHES[(x / 4) as usize];
            let i = ((y * sw + x) * 4) as usize;
            texels[i..i + 4].copy_from_slice(&[f16(p[0]), f16(p[1]), f16(p[2]), f16(1.0)]);
        }
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: sw,
        Height: sh,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        // Like a WGC surface that cannot be sampled: exercises the GPU-side copy too.
        BindFlags: 0,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: texels.as_ptr().cast(), SysMemPitch: sw * 8, SysMemSlicePitch: 0 };
    let mut src = None;
    unsafe { dev.CreateTexture2D(&desc, Some(&init), Some(&mut src)) }.expect("source texture");
    let src = src.unwrap();
    let mut pipe = Pipeline::new(&dev, &ctx).expect("pipeline");
    // Every frame carries its own AVBufferRef; the pool identity must still match (the
    // encoder reopens NVENC when it changes).
    let (a, b) = (pipe.render(&src, sw, sh, (36, 6), 0).unwrap(), pipe.render(&src, sw, sh, (36, 6), 0).unwrap());
    assert_ne!(a.frames_ctx(), b.frames_ctx());
    assert_eq!(a.pool_id(), b.pool_id());
    drop((a, b));
    for mode in [0u32, 1] {
        let frame = pipe.render(&src, sw, sh, (36, 6), mode).expect("render");
        let (w, h, rgba) = read_rgba(&frame).expect("readback");
        assert_eq!((w, h), (36, 6));
        for (i, p) in PATCHES.iter().enumerate() {
            let x = i as u32 * 4 + 1;
            let px = &rgba[((1 * w + x) * 4) as usize..][..4];
            let want = reference(p.map(|v| f16_to_f32(f16(v))), mode);
            for c in 0..3 {
                assert!(px[c].abs_diff(want[c]) <= 1, "mode {mode} patch {i} channel {c}: got {px:?}, want {want:?}");
            }
            assert_eq!(px[3], 255);
        }
        for (x, y) in [(33u32, 1u32), (35, 5), (0, 5)] {
            let px = &rgba[((y * w + x) * 4) as usize..][..3];
            assert_eq!(px, &[0, 0, 0], "outside the source is black at ({x},{y})");
        }
    }
}
