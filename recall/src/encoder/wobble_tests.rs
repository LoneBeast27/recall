//! The real encoder loop on real NVENC, both frame paths: window wobble keeps the replay
//! buffer, a frame too small for NVENC changes nothing, a real resolution change still
//! reopens. A save right after each of them shows how much clip the user would get. The
//! loop runs with an injected saver (no staging folder, no files) and skips without NVENC.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ffmpeg_next as ff;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC};

use super::super::settings::Codec;
use super::super::{run, saver, try_open, Config, EncStats, PacedFrame, SaveRequest};
use crate::framing::{Layout, MIN_ENCODE};
use crate::gpu_frames::{hardware_device, Pipeline};

pub(crate) const BIG: (u32, u32) = (640, 360);
const WOBBLE: (u32, u32) = (640, 358);
/// A window on its way to the taskbar: NVENC refuses this (CS2, 2026-10-31).
const TINY: (u32, u32) = (120, 30);
/// Past the tolerance: keeps its present behaviour on both paths.
const REAL: (u32, u32) = (648, 370);
/// A frame every 1/60 s: 120 frames are 2 s of clip, half of them 1 s.
const FRAME_US: u64 = 16_667;
const PHASE: u64 = 60;

/// One NVENC loop at a time: the tests measure buffers, not session limits.
static NVENC: Mutex<()> = Mutex::new(());

pub(crate) fn nvenc() -> std::sync::MutexGuard<'static, ()> {
    NVENC.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) struct Loop {
    frames: Option<SyncSender<PacedFrame>>,
    saves: Sender<SaveRequest>,
    pub stats: Arc<EncStats>,
    thread: Option<JoinHandle<()>>,
    saver: Option<saver::SaverControl>,
    clip_secs: Receiver<f64>,
    sent: u64,
}

impl Loop {
    fn start() -> Self {
        Self::start_with(Arc::new(EncStats::default()))
    }

    /// With `stats` set up first (faults, the capture-restart hook).
    pub fn start_with(stats: Arc<EncStats>) -> Self {
        let cfg = Config { fps: 60, buffer_secs: 30, bitrate: 2_000_000, ..Config::from_env(false) };
        let (ftx, frx) = sync_channel(3);
        let (stx, srx) = channel();
        let (ctx, crx) = channel();
        let mux: saver::MuxFn = Box::new(move |_, job, _, _| {
            let _ = ctx.send(job.secs);
            Err("wobble test: the clip length is all that is read".into())
        });
        let (queue, control) = saver::spawn(saver::Events::new(false), mux, Box::new(|_, _, _, _| {}));
        let shared = stats.clone();
        let thread = std::thread::spawn(move || run(cfg, frx, srx, shared, Vec::new(), queue));
        Self { frames: Some(ftx), saves: stx, stats, thread: Some(thread), saver: Some(control), clip_secs: crx, sent: 0 }
    }

    pub fn pts(&self) -> u64 {
        self.sent * FRAME_US
    }

    pub fn send(&mut self, frame: PacedFrame) {
        self.frames.as_ref().unwrap().send(frame).expect("encoder loop is alive");
        self.sent += 1;
    }

    pub fn rgba(&mut self, (width, height): (u32, u32)) {
        let rgba = vec![0x40; (width * height * 4) as usize];
        self.send(PacedFrame { idx: self.pts(), width, height, rgba, gpu: None, content: None });
    }

    /// False when this machine has no NVENC (the test then has nothing to say).
    pub fn nvenc_opened(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.stats.backend.load(Relaxed) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let up = self.stats.backend.load(Relaxed) == 1;
        if !up {
            eprintln!("no NVENC here; skipping ({:?})", self.error());
        }
        up
    }

    /// Let NVENC's few frames of delay come out, then save: the clip's length in seconds.
    pub fn clip_after_settling(&self) -> f64 {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut last = (self.stats.encoded_frames.load(Relaxed), Instant::now());
        while Instant::now() < deadline && last.1.elapsed() < Duration::from_millis(300) {
            let now = self.stats.encoded_frames.load(Relaxed);
            if now != last.0 {
                last = (now, Instant::now());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let path = std::env::temp_dir().join(format!("recall-wobble-{}.mp4", uuid::Uuid::new_v4().simple()));
        let save = SaveRequest { path: path.to_string_lossy().into_owned(), seconds: 30, end_us: None, at_last_frame: false, requested: Instant::now() };
        self.saves.send(save).expect("encoder loop is alive");
        // The loop looks at saves between frames, at the latest after its 100 ms wait.
        self.clip_secs.recv_timeout(Duration::from_secs(5)).expect("the save reached the saver")
    }

    pub fn error(&self) -> Option<String> {
        self.stats.error.lock().unwrap().clone()
    }
}

impl Drop for Loop {
    fn drop(&mut self) {
        drop(self.frames.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(saver) = self.saver.take() {
            saver.shutdown(Duration::from_secs(1));
        }
    }
}

/// An Rgba16F capture of `size`, like a WGC surface.
fn surface(dev: &ID3D11Device, (w, h): (u32, u32)) -> ID3D11Texture2D {
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
    let mut tex = None;
    unsafe { dev.CreateTexture2D(&desc, None, Some(&mut tex)) }.expect("capture surface");
    tex.unwrap()
}

/// The capture thread's zero-copy half, minus WGC.
pub(crate) struct Capture {
    dev: ID3D11Device,
    pub pipe: Pipeline,
}

impl Capture {
    pub fn new() -> Option<Self> {
        let (dev, ctx) = hardware_device()?;
        let pipe = Pipeline::new(&dev, &ctx).expect("pipeline");
        Some(Self { dev, pipe })
    }

    /// The frames pool this capture's frames of `size` come from.
    pub fn pool(&mut self, size: (u32, u32)) -> usize {
        let src = surface(&self.dev, size);
        let layout = Layout::recorded(size, None, false, self.pipe.target()).expect("layout");
        self.pipe.render(&src, &layout, 0).expect("render").pool_id()
    }

    pub fn frames(&mut self, enc: &mut Loop, size: (u32, u32), count: u64) {
        let src = surface(&self.dev, size);
        for _ in 0..count {
            let Some(layout) = Layout::recorded(size, None, false, self.pipe.target()) else { continue };
            let gpu = self.pipe.render(&src, &layout, 0).expect("render");
            let (width, height) = (gpu.width(), gpu.height());
            enc.send(PacedFrame { idx: enc.pts(), width, height, rgba: Vec::new(), gpu: Some(gpu), content: layout.content() });
        }
    }
}

#[test]
fn cpu_frames_wobbling_two_rows_keep_the_replay_buffer() {
    let _one = nvenc();
    let mut enc = Loop::start();
    enc.rgba(BIG);
    if !enc.nvenc_opened() {
        return;
    }
    for i in 1..2 * PHASE {
        enc.rgba(if i / 10 % 2 == 1 { WOBBLE } else { BIG });
    }
    let secs = enc.clip_after_settling();
    eprintln!("cpu wobble: clip {secs:.2} s");
    assert!(secs > 1.5, "a save after 11 flips holds {secs:.2} s of 2 s");
    assert_eq!(enc.error(), None);
}

#[test]
fn gpu_frames_wobbling_two_rows_keep_the_replay_buffer_and_the_pool() {
    let _one = nvenc();
    let Some(mut capture) = Capture::new() else { return };
    let mut enc = Loop::start();
    capture.frames(&mut enc, BIG, 1);
    if !enc.nvenc_opened() {
        return;
    }
    capture.frames(&mut enc, BIG, 9);
    for flip in 1..12 {
        capture.frames(&mut enc, if flip % 2 == 1 { WOBBLE } else { BIG }, 10);
    }
    let secs = enc.clip_after_settling();
    eprintln!("gpu wobble: clip {secs:.2} s");
    assert!(secs > 1.5, "a save after 11 flips holds {secs:.2} s of 2 s");
    assert_eq!(capture.pipe.target(), Some(BIG));
    assert_eq!(enc.error(), None);
}

#[test]
fn a_cpu_frame_too_small_for_nvenc_keeps_the_encoder_and_the_buffer() {
    let _one = nvenc();
    let mut enc = Loop::start();
    enc.rgba(BIG);
    if !enc.nvenc_opened() {
        return;
    }
    for _ in 1..PHASE {
        enc.rgba(BIG);
    }
    for _ in 0..5 {
        enc.rgba(TINY);
    }
    for _ in 0..PHASE {
        enc.rgba(BIG);
    }
    let secs = enc.clip_after_settling();
    eprintln!("cpu small frame: clip {secs:.2} s");
    assert!(secs > 1.5, "a save after an alt-tab holds {secs:.2} s of 2 s");
    assert_eq!(enc.error(), None);
}

#[test]
fn a_gpu_frame_too_small_for_nvenc_keeps_zero_copy_and_the_buffer() {
    let _one = nvenc();
    let Some(mut capture) = Capture::new() else { return };
    let mut enc = Loop::start();
    capture.frames(&mut enc, BIG, 1);
    if !enc.nvenc_opened() {
        return;
    }
    capture.frames(&mut enc, BIG, PHASE - 1);
    capture.frames(&mut enc, TINY, 5);
    // Time for the loop to act on the small frames, if any reached it.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!enc.stats.gpu_refused.load(Relaxed), "one small frame took the session off zero-copy: {:?}", enc.error());
    capture.frames(&mut enc, BIG, PHASE);
    let secs = enc.clip_after_settling();
    eprintln!("gpu small frame: clip {secs:.2} s");
    assert!(secs > 1.5, "a save after an alt-tab holds {secs:.2} s of 2 s");
    assert_eq!(capture.pipe.target(), Some(BIG));
    assert_eq!(enc.error(), None);
}

#[test]
fn a_real_resolution_change_still_reopens_and_resets() {
    let _one = nvenc();
    let mut enc = Loop::start();
    enc.rgba(BIG);
    if !enc.nvenc_opened() {
        return;
    }
    for _ in 1..PHASE {
        enc.rgba(BIG);
    }
    for _ in 0..PHASE {
        enc.rgba(REAL);
    }
    let secs = enc.clip_after_settling();
    eprintln!("cpu real change: clip {secs:.2} s");
    assert!(secs < 1.1, "CPU frames: the reopened encoder holds {secs:.2} s, not the old stream");
    drop(enc);

    let Some(mut capture) = Capture::new() else { return };
    let mut enc = Loop::start();
    capture.frames(&mut enc, BIG, PHASE);
    capture.frames(&mut enc, REAL, PHASE);
    let secs = enc.clip_after_settling();
    eprintln!("gpu real change: clip {secs:.2} s");
    assert!(secs < 1.1, "GPU frames: the reopened encoder holds {secs:.2} s, not the old stream");
    assert_eq!(capture.pipe.target(), Some(REAL));
}

/// Smallest even size that opens along one axis.
fn smallest(opens: impl Fn(u32) -> bool) -> u32 {
    let (mut refused, mut opened) = (0u32, 256u32);
    while opened - refused > 2 {
        let mid = ((refused + opened) / 2) & !1;
        if opens(mid) {
            opened = mid;
        } else {
            refused = mid;
        }
    }
    opened
}

#[test]
fn encode_minimum_is_what_the_driver_opens() {
    let _one = nvenc();
    ff::init().unwrap();
    let cfg = Config { fps: 60, buffer_secs: 5, bitrate: 2_000_000, ..Config::from_env(false) };
    let (mut most, mut opened) = ((0, 0), 0);
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        if try_open(&cfg, codec, 256, 256, None).is_err() {
            eprintln!("{} does not open here; skipping it", codec.nvenc());
            continue;
        }
        let w = smallest(|w| try_open(&cfg, codec, w, 256, None).is_ok());
        let h = smallest(|h| try_open(&cfg, codec, 256, h, None).is_ok());
        eprintln!("{} opens from {w}x{h}", codec.nvenc());
        assert!(w <= MIN_ENCODE.0 && h <= MIN_ENCODE.1, "{} needs {w}x{h}, above the rule", codec.nvenc());
        try_open(&cfg, codec, MIN_ENCODE.0, MIN_ENCODE.1, None).expect("each axis alone opens, so both together must");
        most = (most.0.max(w), most.1.max(h));
        opened += 1;
    }
    assert!(opened < 3 || most == MIN_ENCODE, "the codecs open from {most:?}");
}
