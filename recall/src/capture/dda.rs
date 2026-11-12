//! Monitor capture through DXGI Desktop Duplication (`IDXGIOutput5::DuplicateOutput1`), as
//! OBS Display Capture does. Unlike WGC it draws no yellow capture border and needs no
//! Borderless permission (which this PC's Windows build denies to unpackaged apps).
//! Opt-in for now: `RECALL_CAPTURE=dda` (`dda_logic::dda_wanted`).
//!
//! One thread owns the duplication. It waits (high-resolution timer) until the next capture
//! slot, then `AcquireNextFrame` blocks with a timeout; a new image goes straight from the
//! acquired surface down the same `Handler` path as a WGC frame (the GPU path waits until
//! the GPU has read it and copies it itself only when the surface cannot be bound), and the
//! frame is released before the next acquire.
//!
//! While following a game window (`dda_window`), every `dda_logic::FOLLOW_CHECK` the capture
//! looks at it: covered by another window, minimized (exclusive-fullscreen alt-tab, Win+D) or
//! the lock screen hold frames (none reach the encoder, so the clip shows the last game
//! frame and never the window on top); frames resume when the game is back. Only a move to
//! another monitor, or a window that stays off its monitor (`dda_logic::LEAVE_GRACE`), asks
//! for a capture restart (`dda_logic::Follower`).
//!
//! A lost duplication (mode change, exclusive fullscreen switch, UAC secure desktop, monitor
//! sleep or replug) is made again, looking the monitor's output up again each time, on the
//! same device with backoff, so the encoder and replay buffer keep running; a new size flows
//! through the per-frame size handling WGC uses. A lost device, or re-creates failing for
//! longer than `dda_logic::GIVE_UP` while the user's desktop is on screen (the lock screen,
//! UAC and a sleeping display never count: `dda_logic::FailClock`), marks Desktop Duplication failed for the run and asks
//! the engine to restart capture, which then lands on WGC (`CaptureShared::restart_reason`).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_TEXTURE2D_DESC};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Performance::QueryPerformanceFrequency;
use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};

use super::dda_dxgi::{device_on, find_target, present_or_now, Readback, Target, Timer};
use super::dda_logic::{self as logic, Action, FailClock, Follow, Follower, Hold, Pace, Pacer};
use super::{CaptureError, CaptureShared, FrameSource, Handler, Source};
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
    /// Frames not delivered while the followed game was covered, minimized or locked away.
    pub held: AtomicU64,
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
            "[recall] capture: dda stopped ({} frames, {} over the rate cap, {} held, {} re-creates)",
            s.delivered.load(Relaxed),
            s.skipped.load(Relaxed),
            s.held.load(Relaxed),
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

/// Start duplicating `monitor` in the format `shared.fp16` holds. `follow`: a game window
/// that covers this monitor; when it stops covering it the capture asks for a restart (back
/// to WGC window capture). Setup runs on the capture thread (the COM objects stay there);
/// an Err is the reason to use WGC instead.
pub fn start(monitor: HMONITOR, shared: Arc<CaptureShared>, interval: Duration, follow: Option<usize>) -> Result<DdaControl, String> {
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
            let mut cap = match Capture::open(monitor, fp16, shared, stats2, follow) {
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
    monitor: usize,
    target: Target,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dupl: Option<IDXGIOutputDuplication>,
    format: DXGI_FORMAT,
    readback: Readback,
    handler: Handler,
    shared: Arc<CaptureShared>,
    stats: Arc<DdaStats>,
    qpc_frequency: i64,
    timer: Timer,
    follow: Option<usize>,
    /// Why frames are held now (`None`: delivered).
    held: Option<Hold>,
    wrong_format_logged: bool,
}

impl Capture {
    fn open(monitor: usize, fp16: bool, shared: Arc<CaptureShared>, stats: Arc<DdaStats>, follow: Option<usize>) -> Result<Self, String> {
        // DuplicateOutput1 needs per-monitor DPI awareness (as OBS sets for its thread).
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let target = find_target(HMONITOR(monitor as *mut _))?;
        let (device, context) = device_on(&target.adapter)?;
        let mut qpc_frequency = 0i64;
        let _ = unsafe { QueryPerformanceFrequency(&mut qpc_frequency) };
        let format = logic::duplication_format(fp16);
        let mut cap = Self {
            monitor,
            target,
            readback: Readback::new(device.clone(), context.clone(), format),
            device,
            context,
            dupl: None,
            format,
            handler: Handler::from_shared(shared.clone()).paced_elsewhere(),
            shared,
            stats,
            qpc_frequency,
            timer: Timer::new(),
            follow,
            held: None,
            wrong_format_logged: false,
        };
        cap.duplicate()?;
        Ok(cap)
    }

    fn describe(&self) -> String {
        let follow = if self.follow.is_some() { ", following the game window" } else { "" };
        format!("{}, {}{follow}", logic::format_label(self.format), self.target.adapter_name)
    }

    /// Create the duplication of `self.target` on our device, in `self.format`.
    fn duplicate(&mut self) -> Result<(), String> {
        self.dupl = None;
        let wanted = self.format;
        let fp16 = wanted != logic::duplication_format(false);
        let dupl = match self.target.output.cast::<IDXGIOutput5>() {
            Ok(out5) => unsafe { out5.DuplicateOutput1(&self.device, 0, &[wanted]) }.map_err(|e| e.code()),
            Err(e) => Err(e.code()),
        };
        let dupl = match dupl {
            Ok(d) => d,
            Err(code) if logic::legacy_duplication_fits(fp16) => unsafe { self.target.output.DuplicateOutput(&self.device) }
                .map_err(|e| format!("DuplicateOutput: {e} (DuplicateOutput1: {code:?})"))?,
            Err(code) => return Err(format!("DuplicateOutput1 {}: {code:?}", logic::format_label(wanted))),
        };
        // ModeDesc.Format is the display mode (Bgra8 on an SDR desktop even when fp16 frames
        // are asked for); each frame texture's own format is checked in `step`.
        let desc = unsafe { dupl.GetDesc() };
        if !logic::rotation_supported(desc.Rotation) {
            return Err(format!("monitor is rotated ({})", desc.Rotation.0));
        }
        self.dupl = Some(dupl);
        Ok(())
    }

    /// Look the monitor's output up again (sleep/wake, replug and rotation give a new one)
    /// and duplicate it; it must still sit on our device's adapter.
    fn recreate(&mut self) -> Result<(), String> {
        let target = find_target(HMONITOR(self.monitor as *mut _))?;
        if target.luid != self.target.luid {
            return Err(format!("monitor moved to {}", target.adapter_name));
        }
        self.target = target;
        self.duplicate()
    }

    /// Desktop Duplication cannot go on: mark it failed for the run and ask the engine to
    /// restart capture (on WGC).
    fn give_up(&self, why: &str) {
        eprintln!("[recall] capture: dda {why}; capture restarts on WGC");
        self.shared.dda_failed.store(true, Relaxed);
        self.shared.restart.store(true, Relaxed);
    }

    fn run(&mut self, stop: &AtomicBool, interval: Duration) {
        let mut pacer = Pacer::new(interval);
        let (mut first, mut attempt, mut fails) = (true, 0u32, FailClock::default());
        let (mut follower, mut followed) = (Follower::default(), None::<Instant>);
        while !stop.load(Relaxed) {
            if let Some(hwnd) = self.follow {
                if followed.is_none_or(|t| t.elapsed() >= logic::FOLLOW_CHECK) {
                    followed = Some(Instant::now());
                    let state = super::dda_window::snapshot(hwnd, self.monitor);
                    let verdict = follower.decide(&state, self.monitor, super::dda_desktop::now(), Instant::now());
                    match apply_follow(&self.shared, &mut self.held, verdict) {
                        Applied::Ended => return,
                        Applied::Resumed => first = true,
                        Applied::Same => {}
                    }
                }
            }
            if self.dupl.is_none() {
                if fails.give_up(Instant::now(), super::dda_desktop::now()) {
                    return self.give_up("re-create kept failing");
                }
                if !sleep_unless_stopped(logic::backoff(attempt), stop) {
                    return;
                }
                attempt = attempt.saturating_add(1);
                match self.recreate() {
                    Ok(()) => {
                        eprintln!("[recall] capture: dda duplication re-created");
                        self.stats.recreated.fetch_add(1, Relaxed);
                        (attempt, first) = (0, true);
                        fails.recovered();
                    }
                    Err(e) if attempt == 1 || attempt.is_multiple_of(20) => eprintln!("[recall] capture: dda re-create failed ({e}), retrying"),
                    Err(_) => {}
                }
                continue;
            }
            // Acquire no faster than the capture rate: the wait lets frames above the cap
            // accumulate in the duplication instead of being acquired and dropped.
            self.timer.wait(pacer.until_due(Instant::now()));
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
                    Action::Fatal => return self.give_up(&format!("device lost ({code:?})")),
                },
            }
        }
    }

    /// Acquire one frame and deliver or skip it, releasing it before returning. Ok(true): a
    /// new image was taken.
    fn step(&mut self, pacer: &mut Pacer, first: bool) -> Result<bool, windows::core::HRESULT> {
        let dupl = self.dupl.clone().ok_or(DXGI_ERROR_ACCESS_LOST)?;
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        unsafe { dupl.AcquireNextFrame(ACQUIRE_TIMEOUT_MS, &mut info, &mut resource) }.map_err(|e| e.code())?;
        self.stats.acquired.fetch_add(1, Relaxed);
        let took = self.take(&info, resource, pacer, first);
        let _ = unsafe { dupl.ReleaseFrame() };
        Ok(took)
    }

    /// Hand an acquired frame down the `Handler` path, straight from the acquired surface.
    fn take(&mut self, info: &DXGI_OUTDUPL_FRAME_INFO, resource: Option<IDXGIResource>, pacer: &mut Pacer, first: bool) -> bool {
        if !logic::has_new_image(info.LastPresentTime, info.AccumulatedFrames, first) {
            self.stats.no_image.fetch_add(1, Relaxed);
            return false;
        }
        if self.held.is_some() {
            // Covered, minimized or locked away: nothing reaches the encoder (the stream is
            // variable rate: the last game frame lasts until the next one), and the capture
            // keeps running for the return.
            self.stats.held.fetch_add(1, Relaxed);
            return false;
        }
        if let Pace::Skip(_) = pacer.offer(Instant::now()) {
            self.stats.skipped.fetch_add(1, Relaxed);
            return false;
        }
        let Some(texture) = resource.and_then(|r| r.cast::<ID3D11Texture2D>().ok()) else { return false };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let size = (desc.Width, desc.Height);
        if let Ok(mut f) = self.stats.format.lock() {
            *f = logic::format_label(desc.Format);
        }
        if desc.Format != self.format {
            if !std::mem::replace(&mut self.wrong_format_logged, true) {
                eprintln!("[recall] capture: dda frame format {} (asked {}); frame dropped", desc.Format.0, logic::format_label(self.format));
            }
            return false;
        }
        let timestamp = logic::qpc_to_100ns(present_or_now(info.LastPresentTime), self.qpc_frequency);
        self.stats.delivered.fetch_add(1, Relaxed);
        if let Ok(mut s) = self.stats.size.lock() {
            *s = size;
        }
        let mut frame = DdaFrame { device: &self.device, context: &self.context, texture: &texture, size, timestamp, readback: &mut self.readback };
        if let Err(e) = self.handler.deliver(&mut frame) {
            eprintln!("[recall] capture: dda frame failed: {e}");
        }
        true
    }
}

/// What a follow verdict did to the capture.
#[derive(Debug, PartialEq, Eq)]
enum Applied {
    /// No change (still delivering, or still held).
    Same,
    /// Frames flow again: take the next frame even if it brings no new present.
    Resumed,
    /// A capture restart was asked; the capture thread ends.
    Ended,
}

/// Apply a `Follower` verdict: hold or resume frames (logged on change), or ask the engine
/// for a capture restart. Holding never touches `restart`, so the encoder and the replay
/// buffer keep running.
fn apply_follow(shared: &CaptureShared, held: &mut Option<Hold>, verdict: Follow) -> Applied {
    let now = match verdict {
        Follow::Deliver => None,
        Follow::Hold(why) => Some(why),
        Follow::Restart(why) => {
            eprintln!("[recall] capture: {why}; capture restarts");
            shared.restart.store(true, Relaxed);
            return Applied::Ended;
        }
    };
    if now == *held {
        return Applied::Same;
    }
    let was = std::mem::replace(held, now);
    match now {
        Some(why) => {
            eprintln!("[recall] capture: dda holding frames ({})", why.label());
            Applied::Same
        }
        None => {
            eprintln!("[recall] capture: dda frames resume ({} ended)", was.map_or("hold", Hold::label));
            Applied::Resumed
        }
    }
}

/// An acquired Desktop Duplication frame as a `FrameSource`: the capture's device and
/// context and the acquired texture, borrowed for the call.
struct DdaFrame<'a> {
    device: &'a ID3D11Device,
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
    size: (u32, u32),
    timestamp: i64,
    readback: &'a mut Readback,
}

impl FrameSource for DdaFrame<'_> {
    fn source(&self) -> Source<'_> {
        Source { device: self.device, context: self.context, texture: self.texture, size: self.size, timestamp: self.timestamp }
    }

    fn read(&mut self, rect: Rect) -> Result<Vec<u8>, CaptureError> {
        self.readback.read(self.texture, self.size, rect)
    }
}

/// Sleep `total` in short steps; false once `stop` is set.
fn sleep_unless_stopped(total: Duration, stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + total;
    while !stop.load(Relaxed) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(Duration::from_millis(25)));
    }
    false
}

#[cfg(test)]
#[path = "dda_tests.rs"]
mod tests;
