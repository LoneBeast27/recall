//! Size policy, plus the shader checked on the real GPU against a CPU reference of the same
//! math (tonemap.wgsl's passthrough / knee tonemap + sRGB encode), through the same FFmpeg
//! frames pool and readback the recorder uses.

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC};

use super::{hardware_device, read_rgba, target_size, Pipeline};
use crate::framing::{Layout, Rect};

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
    let layout = Layout::native((sw, sh), (36, 6));
    let (a, b) = (pipe.render(&src, &layout, 0).unwrap(), pipe.render(&src, &layout, 0).unwrap());
    assert_ne!(a.frames_ctx(), b.frames_ctx());
    assert_eq!(a.pool_id(), b.pool_id());
    drop((a, b));
    for mode in [0u32, 1] {
        let frame = pipe.render(&src, &layout, mode).expect("render");
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

/// An Rgba16F texture whose texel (x, y) is `f(x, y)` (scRGB), without the shader-resource
/// bind flag, like a WGC surface.
fn texture(dev: &ID3D11Device, w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 3]) -> ID3D11Texture2D {
    let mut texels = vec![0u16; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let c = f(x, y);
            let i = ((y * w + x) * 4) as usize;
            texels[i..i + 4].copy_from_slice(&[f16(c[0]), f16(c[1]), f16(c[2]), f16(1.0)]);
        }
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: 0,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: texels.as_ptr().cast(), SysMemPitch: w * 8, SysMemSlicePitch: 0 };
    let mut tex = None;
    unsafe { dev.CreateTexture2D(&desc, Some(&init), Some(&mut tex)) }.expect("texture");
    tex.unwrap()
}

#[test]
fn windowed_client_area_lands_on_the_canvas_pixel_exact_with_black_bars() {
    let Some((dev, ctx)) = hardware_device() else {
        eprintln!("no hardware D3D11 device; skipping");
        return;
    };
    // A 16x10 "window": 3 white title-bar rows, red 1 px side borders, and a client area
    // whose texels all differ.
    let client = |x: u32, y: u32| [x as f32 / 16.0, (y - 3) as f32 / 8.0, 0.25];
    let src = texture(&dev, 16, 10, |x, y| match (x, y) {
        (_, 0..=2) => [1.0, 1.0, 1.0],
        (0 | 15, _) => [1.0, 0.0, 0.0],
        _ => client(x, y),
    });
    let crop = Rect { x: 1, y: 3, w: 14, h: 7 };
    let layout = Layout::on_canvas(crop, (20, 12));
    assert_eq!(layout.dst, Rect { x: 2, y: 2, w: 14, h: 7 });
    let mut pipe = Pipeline::new(&dev, &ctx).expect("pipeline");
    let frame = pipe.render(&src, &layout, 0).expect("render");
    let (w, h, rgba) = read_rgba(&frame).expect("readback");
    assert_eq!((w, h), (20, 12));
    let d = layout.dst;
    for y in 0..h {
        for x in 0..w {
            let px = &rgba[((y * w + x) * 4) as usize..][..3];
            let inside = x >= d.x && x < d.x + d.w && y >= d.y && y < d.y + d.h;
            if !inside {
                assert_eq!(px, &[0, 0, 0], "bar pixel ({x},{y}) is exactly black");
                continue;
            }
            let (sx, sy) = (crop.x + x - d.x, crop.y + y - d.y);
            let want = reference(client(sx, sy).map(|v| f16_to_f32(f16(v))), 0);
            for c in 0..3 {
                assert!(px[c].abs_diff(want[c]) <= 1, "({x},{y}) <- texel ({sx},{sy}): got {px:?}, want {want:?}");
            }
        }
    }
    // A resize keeps the canvas: same pool, so NVENC does not reopen.
    let smaller = Layout::on_canvas(Rect { x: 1, y: 3, w: 10, h: 5 }, (20, 12));
    let again = pipe.render(&src, &smaller, 0).expect("render");
    assert_eq!(again.pool_id(), frame.pool_id());
}

#[test]
fn a_client_area_larger_than_the_canvas_is_scaled_down_without_title_bar_bleed() {
    let Some((dev, ctx)) = hardware_device() else {
        eprintln!("no hardware D3D11 device; skipping");
        return;
    };
    // 32x16 window: 4 white title rows over a client of red (left half) | blue (right).
    let src = texture(&dev, 32, 16, |x, y| match (x, y) {
        (_, 0..=3) => [1.0, 1.0, 1.0],
        (0..=15, _) => [1.0, 0.0, 0.0],
        _ => [0.0, 0.0, 1.0],
    });
    let layout = Layout::on_canvas(Rect { x: 0, y: 4, w: 32, h: 12 }, (16, 8));
    assert_eq!(layout.dst, Rect { x: 0, y: 0, w: 16, h: 6 }, "32x12 halves to 16x6, centred");
    let mut pipe = Pipeline::new(&dev, &ctx).expect("pipeline");
    let (w, h, rgba) = read_rgba(&pipe.render(&src, &layout, 0).expect("render")).expect("readback");
    assert_eq!((w, h), (16, 8));
    let px = |x: u32, y: u32| <[u8; 3]>::try_from(&rgba[((y * w + x) * 4) as usize..][..3]).unwrap();
    for y in 0..6 {
        assert_eq!(px(1, y), [255, 0, 0], "row {y}: red, no white from the title bar");
        assert_eq!(px(14, y), [0, 0, 255], "row {y}: blue");
    }
    for x in 0..16 {
        assert_eq!((px(x, 6), px(x, 7)), ([0, 0, 0], [0, 0, 0]), "bottom bar black at column {x}");
    }
}
