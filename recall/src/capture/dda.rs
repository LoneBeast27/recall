//! Monitor capture through DXGI Desktop Duplication (`IDXGIOutput5::DuplicateOutput1`), as
//! OBS Display Capture does. Unlike WGC it draws no yellow capture border and needs no
//! Borderless permission (which this PC's Windows build denies to unpackaged apps).
//!
//! One thread owns the duplication: `AcquireNextFrame` blocks with a timeout, a new image is
//! copied once into our own shader-readable texture (`CopyResource`), the frame is released
//! at once, and the copy goes down the same `Handler` path as a WGC frame (zero-copy GPU
//! tonemap -> NVENC, or the CPU readback fallback). Frames above the capture rate are
//! released unread. A lost duplication (mode change, exclusive fullscreen switch, UAC secure
//! desktop) is made again on the same device with backoff, so the encoder and replay buffer
//! keep running; a new size flows through the per-frame size handling WGC uses.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};

use super::dda_logic::{self as logic, Action, Pace, Pacer};
use super::{CaptureError, CaptureShared, Handler, Source};
use crate::framing::Rect;

/// `AcquireNextFrame` timeout: how often an idle capture checks for stop.
const ACQUIRE_TIMEOUT_MS: u32 = 100;

/// Counters for logs and the headless check.
#[derive(Default)]
pub struct DdaStats {
    pub delivered: AtomicU64,
    pub skipped: AtomicU64,
    pub recreated: AtomicU32,
    /// Acquires that returned a frame / that timed out on an idle desktop.
    pub acquired: AtomicU64,
    pub idle: AtomicU64,
    /// Frames with no new image (mouse-only updates).
    pub no_image: AtomicU64,
    pub size: Mutex<(u32, u32)>,
    pub format: Mutex<&'static str>,
}

/// Keeps the duplication thread alive; `stop` (or drop) ends it.
pub struct DdaControl {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub stats: Arc<DdaStats>,
}

impl DdaControl {
    pub fn stop(mut self) -> Result<(), String> {
        self.halt()
    }

    fn halt(&mut self) -> Result<(), String> {
        self.stop.store(true, Relaxed);
        let Some(thread) = self.thread.take() else { return Ok(()) };
        let joined = thread.join().map_err(|_| "dda capture thread panicked".to_string());
        let s = &self.stats;
        eprintln!(
            "[recall] capture: dda stopped ({} frames, {} over the rate cap, {} re-creates)",
            s.delivered.load(Relaxed),
            s.skipped.load(Relaxed),
            s.recreated.load(Relaxed)
        );
        joined
    }
}

impl Drop for DdaControl {
    fn drop(&mut self) {
        let _ = self.halt();
    }
}

/// The output that shows `monitor` and the adapter that drives it.
struct Target {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput1,
    adapter_name: String,
}

/// Start duplicating `monitor` in the format `shared.fp16` holds. Setup runs on the capture
/// thread (the COM objects stay there); an Err is the reason to use WGC instead.
pub fn start(monitor: HMONITOR, shared: Arc<CaptureShared>, interval: Duration) -> Result<DdaControl, String> {
    let fp16 = shared.fp16.load(Relaxed);
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(DdaStats::default());
    let (ready_tx, ready_rx) = mpsc::channel::<Result<String, String>>();
    let (stop2, stats2) = (stop.clone(), stats.clone());
    // An HMONITOR is a plain handle value; pass it as an integer (raw pointers are not Send).
    let monitor = monitor.0 as usize;
    let thread = std::thread::Builder::new()
        .name("recall-dda".into())
        .spawn(move || {
            let mut cap = match Capture::open(HMONITOR(monitor as *mut _), fp16, shared, stats2) {
                Ok(cap) => cap,
                Err(e) => return drop(ready_tx.send(Err(e))),
            };
            let _ = ready_tx.send(Ok(cap.describe()));
            cap.run(&stop2, interval);
        })
        .map_err(|e| format!("thread: {e}"))?;
    match ready_rx.recv() {
        Ok(Ok(line)) => {
            eprintln!("[recall] capture: dda ({line})");
            Ok(DdaControl { stop, thread: Some(thread), stats })
        }
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => Err("dda thread ended during setup".into()),
    }
}

/// The capture thread's state.
struct Capture {
    target: Target,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dupl: Option<IDXGIOutputDuplication>,
    fp16: bool,
    format: DXGI_FORMAT,
    /// Our shader-readable copy of the desktop and the CPU-readback staging texture.
    copy: Option<(ID3D11Texture2D, (u32, u32))>,
    readback: Readback,
    handler: Handler,
    stats: Arc<DdaStats>,
    qpc_frequency: i64,
    wrong_format_logged: bool,
}

impl Capture {
    fn open(monitor: HMONITOR, fp16: bool, shared: Arc<CaptureShared>, stats: Arc<DdaStats>) -> Result<Self, String> {
        // DuplicateOutput1 needs per-monitor DPI awareness (as OBS sets for its thread).
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let target = find_target(monitor)?;
        let (device, context) = device_on(&target.adapter)?;
        let mut qpc_frequency = 0i64;
        let _ = unsafe { QueryPerformanceFrequency(&mut qpc_frequency) };
        let mut cap = Self {
            target,
            device: device.clone(),
            context: context.clone(),
            dupl: None,
            fp16,
            format: logic::duplication_format(fp16),
            copy: None,
            readback: Readback { device: device.clone(), context: context.clone(), format: logic::duplication_format(fp16), staging: None },
            handler: Handler::from_shared(shared),
            stats,
            qpc_frequency,
            wrong_format_logged: false,
        };
        cap.duplicate()?;
        Ok(cap)
    }

    fn describe(&self) -> String {
        format!("{}, {}", logic::format_label(self.format), self.target.adapter_name)
    }

    /// (Re)create the duplication on our device.
    fn duplicate(&mut self) -> Result<(), String> {
        self.dupl = None;
        let wanted = logic::duplication_format(self.fp16);
        let dupl = match self.target.output.cast::<IDXGIOutput5>() {
            Ok(out5) => unsafe { out5.DuplicateOutput1(&self.device, 0, &[wanted]) }.map_err(|e| e.code()),
            Err(e) => Err(e.code()),
        };
        let dupl = match dupl {
            Ok(d) => d,
            Err(code) if logic::legacy_duplication_fits(self.fp16) => unsafe { self.target.output.DuplicateOutput(&self.device) }
                .map_err(|e| format!("DuplicateOutput: {e} (DuplicateOutput1: {code:?})"))?,
            Err(code) => return Err(format!("DuplicateOutput1 {}: {code:?}", logic::format_label(wanted))),
        };
        // ModeDesc.Format is the display mode (Bgra8 on an SDR desktop even when fp16 frames
        // are asked for); the frame texture's own format is checked in `copy_out`.
        let desc = unsafe { dupl.GetDesc() };
        if !logic::rotation_supported(desc.Rotation) {
            return Err(format!("monitor is rotated ({})", desc.Rotation.0));
        }
        self.format = wanted;
        self.readback.format = wanted;
        self.dupl = Some(dupl);
        Ok(())
    }

    fn run(&mut self, stop: &AtomicBool, interval: Duration) {
        let mut pacer = Pacer::new(interval);
        let mut first = true;
        let mut attempt = 0u32;
        while !stop.load(Relaxed) {
            if self.dupl.is_none() {
                std::thread::sleep(logic::backoff(attempt));
                attempt = attempt.saturating_add(1);
                match self.duplicate() {
                    Ok(()) => {
                        eprintln!("[recall] capture: dda duplication re-created");
                        self.stats.recreated.fetch_add(1, Relaxed);
                        attempt = 0;
                        first = true;
                    }
                    Err(e) if attempt == 1 || attempt.is_multiple_of(20) => eprintln!("[recall] capture: dda re-create failed ({e}), retrying"),
                    Err(_) => {}
                }
                continue;
            }
            match self.step(&mut pacer, first) {
                Ok(took) => first &= !took,
                Err(code) => match logic::action_for(code) {
                    Action::Idle => {
                        self.stats.idle.fetch_add(1, Relaxed);
                    }
                    Action::Recreate => {
                        eprintln!("[recall] capture: dda duplication lost ({code:?}); re-creating");
                        self.dupl = None;
                    }
                    Action::Fatal => {
                        eprintln!("[recall] capture: dda device lost ({code:?}); capture stops");
                        return;
                    }
                },
            }
        }
    }

    /// Acquire one frame and deliver or skip it. Ok(true): a new image was taken.
    fn step(&mut self, pacer: &mut Pacer, first: bool) -> Result<bool, windows::core::HRESULT> {
        let dupl = self.dupl.clone().ok_or(DXGI_ERROR_ACCESS_LOST)?;
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        unsafe { dupl.AcquireNextFrame(ACQUIRE_TIMEOUT_MS, &mut info, &mut resource) }.map_err(|e| e.code())?;
        self.stats.acquired.fetch_add(1, Relaxed);
        if !logic::has_new_image(info.LastPresentTime, info.AccumulatedFrames, first) {
            self.stats.no_image.fetch_add(1, Relaxed);
            let _ = unsafe { dupl.ReleaseFrame() };
            return Ok(false);
        }
        if let Pace::Skip(wait) = pacer.offer(Instant::now()) {
            let _ = unsafe { dupl.ReleaseFrame() };
            self.stats.skipped.fetch_add(1, Relaxed);
            // The next acquire returns the newest frame presented meanwhile.
            precise_wait(wait);
            return Ok(false);
        }
        let copied = resource.and_then(|r| r.cast::<ID3D11Texture2D>().ok()).and_then(|t| self.copy_out(&t));
        let _ = unsafe { dupl.ReleaseFrame() };
        let Some((texture, size)) = copied else { return Ok(false) };
        let timestamp = logic::qpc_to_100ns(present_or_now(info.LastPresentTime), self.qpc_frequency);
        self.stats.delivered.fetch_add(1, Relaxed);
        if let Ok(mut s) = self.stats.size.lock() {
            *s = size;
        }
        let (device, context) = (self.device.clone(), self.context.clone());
        let src = Source { device: &device, context: &context, texture: &texture, size, timestamp };
        let readback = &mut self.readback;
        let mut cpu = |rect: Rect| readback.read(&texture, size, rect);
        if let Err(e) = self.handler.deliver(&src, &mut cpu) {
            eprintln!("[recall] capture: dda frame failed: {e}");
        }
        Ok(true)
    }

    /// One `CopyResource` of the desktop image into our own texture (made again on a size
    /// change); the duplication frame can then be released at once.
    fn copy_out(&mut self, desktop: &ID3D11Texture2D) -> Option<(ID3D11Texture2D, (u32, u32))> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { desktop.GetDesc(&mut desc) };
        let size = (desc.Width, desc.Height);
        if let Ok(mut f) = self.stats.format.lock() {
            *f = logic::format_label(desc.Format);
        }
        if desc.Format != self.format {
            if !std::mem::replace(&mut self.wrong_format_logged, true) {
                eprintln!("[recall] capture: dda frame format {} (asked {}); frame dropped", desc.Format.0, logic::format_label(self.format));
            }
            return None;
        }
        if self.copy.as_ref().is_none_or(|(_, s)| *s != size) {
            let want = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut tex = None;
            if let Err(e) = unsafe { self.device.CreateTexture2D(&want, None, Some(&mut tex)) } {
                eprintln!("[recall] capture: dda copy texture {}x{}: {e}", size.0, size.1);
                return None;
            }
            self.copy = Some((tex?, size));
        }
        let (tex, _) = self.copy.as_ref()?;
        unsafe { self.context.CopyResource(tex, desktop) };
        Some((tex.clone(), size))
    }
}

/// The CPU-readback staging texture (made again on a size change).
struct Readback {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    format: DXGI_FORMAT,
    staging: Option<(ID3D11Texture2D, (u32, u32))>,
}

impl Readback {
    /// CPU path only (no encoder GPU sink, or the dump harness): map a staging copy and copy
    /// out `rect` of the fp16 frame (8 bytes/pixel), as WGC's `frame.buffer()` path does.
    fn read(&mut self, src: &ID3D11Texture2D, size: (u32, u32), rect: Rect) -> Result<Vec<u8>, CaptureError> {
        if self.staging.as_ref().is_none_or(|(_, s)| *s != size) {
            let want = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: self.format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut tex = None;
            unsafe { self.device.CreateTexture2D(&want, None, Some(&mut tex))? };
            self.staging = Some((tex.ok_or("staging texture missing")?, size));
        }
        let (staging, _) = self.staging.as_ref().ok_or("staging texture missing")?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context.CopyResource(staging, src);
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let pitch = mapped.RowPitch as usize;
        let data = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * size.1 as usize) };
        let bytes = crate::framing::copy_rect(data, pitch, 8, rect);
        unsafe { self.context.Unmap(staging, 0) };
        Ok(bytes)
    }
}

fn present_or_now(last_present_time: i64) -> i64 {
    if last_present_time != 0 {
        return last_present_time;
    }
    let mut now = 0i64;
    let _ = unsafe { QueryPerformanceCounter(&mut now) };
    now
}

/// Find the output showing `monitor`. Its adapter must be the one WGC (and so the encoder
/// path) would use, the process default adapter: on a hybrid laptop where the iGPU drives
/// the panel and the dGPU encodes (`hybrid_gpu`), DDA would need a cross-adapter copy, so
/// those stay on WGC.
fn find_target(monitor: HMONITOR) -> Result<Target, String> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| format!("DXGI factory: {e}"))?;
    let default = unsafe { factory.EnumAdapters1(0) }.and_then(|a| unsafe { a.GetDesc1() }).map_err(|e| format!("default adapter: {e}"))?;
    for a in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(a) }) else { break };
        for o in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(o) }) else { break };
            let Ok(desc) = (unsafe { output.GetDesc() }) else { continue };
            if desc.Monitor != monitor {
                continue;
            }
            let ad = unsafe { adapter.GetDesc1() }.map_err(|e| format!("adapter desc: {e}"))?;
            let name = wide(&ad.Description);
            if ad.AdapterLuid != default.AdapterLuid {
                return Err(format!("monitor is driven by {name}, the encoder device is on {} (hybrid GPU)", wide(&default.Description)));
            }
            let output = output.cast::<IDXGIOutput1>().map_err(|e| format!("IDXGIOutput1: {e}"))?;
            return Ok(Target { adapter, output, adapter_name: name });
        }
    }
    Err("no DXGI output shows this monitor".into())
}

fn device_on(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| format!("D3D11 device: {e}"))?;
    let (device, context) = (device.ok_or("no device")?, context.ok_or("no context")?);
    // The encoder and screenshot readbacks share the device across threads (as on WGC).
    if let Ok(mt) = context.cast::<ID3D11Multithread>() {
        let _ = unsafe { mt.SetMultithreadProtected(true) };
    }
    Ok((device, context))
}

fn wide(s: &[u16]) -> String {
    String::from_utf16_lossy(&s[..s.iter().position(|c| *c == 0).unwrap_or(s.len())])
}

/// Sleep `wait` with a high-resolution waitable timer (plain sleep rounds up to the 15.6 ms
/// system tick, a whole frame at 60 fps).
fn precise_wait(wait: Duration) {
    use windows::Win32::System::Threading::{
        CreateWaitableTimerExW, SetWaitableTimer, WaitForSingleObject, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE,
        TIMER_ALL_ACCESS,
    };
    thread_local! {
        static TIMER: Option<windows::Win32::Foundation::HANDLE> = unsafe {
            CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0).ok()
        };
    }
    if wait.is_zero() {
        return;
    }
    let due = -((wait.as_nanos() / 100) as i64).max(1); // negative = relative, 100 ns units
    let waited = TIMER.with(|t| match t {
        Some(h) => unsafe { SetWaitableTimer(*h, &due, 0, None, None, false).is_ok() && WaitForSingleObject(*h, INFINITE).0 == 0 },
        None => false,
    });
    if !waited {
        std::thread::sleep(wait);
    }
}

#[cfg(test)]
#[path = "dda_tests.rs"]
mod tests;
