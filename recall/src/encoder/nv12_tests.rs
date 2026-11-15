//! BGRA-fed vs NV12-fed NVENC on the real GPU: the same colour bars through the old pipeline
//! (NVENC converts BGRA) and the NV12 one (the video processor converts), saved, decoded with
//! FFmpeg, and the decoded Y/Cb/Cr of every bar compared with each other and with exact BT.601
//! studio range. Also the stream tags match. Skips (says so) without a GPU or NVENC.

use std::time::Instant;

use ffmpeg_next as ff;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use super::replay::Replay;
use super::{open_encoder, saver, Config, EncStats, PacedFrame, SaveRequest};
use crate::framing::Layout;
use crate::gpu_frames::Pipeline;

const W: u32 = 512;
const H: u32 = 256;
/// sRGB bars, as `gpu_frames::nv12_check`.
const BARS: [[u8; 3]; 8] = [[255, 255, 255], [255, 255, 0], [0, 255, 255], [0, 255, 0], [255, 0, 255], [255, 0, 0], [0, 0, 255], [0, 0, 0]];

fn bt601([r, g, b]: [u8; 3]) -> [f32; 3] {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    [16.0 + 219.0 * y, 128.0 + 224.0 * (b - y) / 1.772, 128.0 + 224.0 * (r - y) / 1.402]
}

fn bars(dev: &ID3D11Device) -> ID3D11Texture2D {
    let px: Vec<u8> = (0..W * H).flat_map(|i| { let [r, g, b] = BARS[((i % W) * 8 / W) as usize]; [b, g, r, 255] }).collect();
    let desc = D3D11_TEXTURE2D_DESC {
        Width: W,
        Height: H,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        // Like a WGC surface (bind 0x28).
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr().cast(), SysMemPitch: W * 4, SysMemSlicePitch: 0 };
    let mut tex = None;
    unsafe { dev.CreateTexture2D(&desc, Some(&init), Some(&mut tex)) }.expect("bars texture");
    tex.unwrap()
}

/// Encode 30 frames of `src` through `pipe` and save them; None when NVENC cannot open here.
fn clip(pipe: &mut Pipeline, src: &ID3D11Texture2D, dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let cfg = Config { fps: 60, buffer_secs: 10, ..Config::from_env(false) };
    let stats = EncStats::default();
    let mut replay = Replay::new(&cfg, None);
    let mut enc: Option<super::Enc> = None;
    for i in 0..30u64 {
        let gpu = pipe.render(src, &Layout::native((W, H), (W, H)), 0).expect("render");
        let frame = PacedFrame { idx: i * 1_000_000 / 60, width: W, height: H, rgba: Vec::new(), gpu: Some(gpu), content: None };
        if enc.is_none() {
            match open_encoder(&cfg, super::settings::Codec::H264, W, H, frame.gpu.as_ref()) {
                Ok((opened, _)) => {
                    replay.begin(&opened);
                    enc = Some(opened);
                }
                Err(e) => {
                    assert!(std::env::var_os("RECALL_REQUIRE_NVENC").is_none(), "NVENC required: {e}");
                    crate::hw_skip::skipped(&format!("NVENC did not open: {e}"));
                    return None;
                }
            }
        }
        enc.as_mut().unwrap().encode(frame, &stats, &mut replay).expect("encode");
    }
    let mut enc = enc.unwrap();
    enc.encoder.send_eof().expect("flush");
    enc.drain(&stats, &mut replay).expect("drain");
    let path = dir.join(format!("{name}.mp4"));
    let req = SaveRequest { path: path.to_string_lossy().into_owned(), game: None, seconds: 1, end_us: None, at_last_frame: false, requested: Instant::now() };
    saver::write_now(&replay.build_job(req, &[], Instant::now()).expect("job")).expect("save");
    Some(path)
}

/// The last decoded frame's Y/Cb/Cr at each bar's centre, and the stream's (matrix, range).
fn decode(path: &std::path::Path) -> ([[f32; 3]; 8], (ff::color::Space, ff::color::Range)) {
    let mut ictx = ff::format::input(&path).expect("clip opens");
    let vi = ictx.streams().best(ff::media::Type::Video).expect("video").index();
    let params = ictx.stream(vi).unwrap().parameters();
    let mut dec = ff::codec::context::Context::from_parameters(params).unwrap().decoder().video().expect("decoder");
    let (mut frame, mut last) = (ff::frame::Video::empty(), None);
    for (stream, packet) in ictx.packets() {
        if stream.index() == vi {
            dec.send_packet(&packet).expect("decode");
            while dec.receive_frame(&mut frame).is_ok() {
                last = Some(frame.clone());
            }
        }
    }
    dec.send_eof().ok();
    while dec.receive_frame(&mut frame).is_ok() {
        last = Some(frame.clone());
    }
    let f = last.expect("a decoded frame");
    let at = |plane: usize, x: usize, y: usize| f.data(plane)[y * f.stride(plane) + x] as f32;
    let mut out = [[0f32; 3]; 8];
    for (i, v) in out.iter_mut().enumerate() {
        let x = (i * W as usize / 8) + W as usize / 16;
        *v = [at(0, x, H as usize / 2), at(1, x / 2, H as usize / 4), at(2, x / 2, H as usize / 4)];
    }
    (out, (dec.color_space(), dec.color_range()))
}

#[test]
fn nv12_fed_nvenc_gives_the_colours_and_tags_of_bgra_fed_nvenc() {
    let Some((dev, ctx)) = crate::hw_skip::gpu_device() else { return };
    ff::init().unwrap();
    let dir = super::test_scratch::scratch("recall-nv12-colours", "bars");
    let src = bars(&dev);
    let mut old = Pipeline::new_bgra(&dev, &ctx).expect("BGRA pipeline");
    let mut new = Pipeline::new(&dev, &ctx).expect("NV12 pipeline");
    assert!(new.nv12(), "the NV12 stage starts here (its colour check passed)");
    let Some(a) = clip(&mut old, &src, &dir, "bgra") else { return };
    let Some(b) = clip(&mut new, &src, &dir, "nv12") else { return };
    let ((va, ta), (vb, tb)) = (decode(&a), decode(&b));
    assert_eq!(ta, tb, "same matrix and range tags");
    for (i, bar) in BARS.iter().enumerate() {
        let want = bt601(*bar);
        for c in 0..3 {
            assert!((va[i][c] - vb[i][c]).abs() <= 2.0, "bar {i} component {c}: BGRA-fed {:?} vs NV12-fed {:?}", va[i], vb[i]);
            assert!((vb[i][c] - want[c]).abs() <= 2.5, "bar {i} component {c}: NV12-fed {:?} vs exact BT.601 {want:?}", vb[i]);
        }
    }
}
