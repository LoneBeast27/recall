//! A short real-time ETW session (`RecallDetectRender-<session>`) that watches how one
//! process presents while it waits to be committed as a game
//! (`record_render`). It runs only while such a candidate is pending, then is
//! dropped, which stops it. Nothing is injected: the D3D runtime, the graphics
//! kernel and win32k already log these events; the session only subscribes.
//!
//! Events (ids and payloads from Intel PresentMon, MIT, `PresentData/ETW/*.h`
//! and `PresentMonTraceConsumer.cpp`, read 2026-11-05; field names checked
//! against this PC's decoded trace):
//! - DXGI `Present_Start` (42) / `PresentMultiplaneOverlay_Start` (55):
//!   `pIDXGISwapChain`, `Flags` (tearing = `DXGI_PRESENT_ALLOW_TEARING`).
//! - D3D9 `Present_Start` (1).
//! - DxgKrnl `Present_Info` (184, any API) and `Flip_Info` (168, the legacy
//!   flip of exclusive fullscreen), both on the presenting thread.
//! - Win32k `TokenCompositionSurfaceObject` (201, on the presenting thread:
//!   the process's composition surface) and `TokenStateChanged` (301, logged
//!   by DWM): `NewState` 3 = the frame goes on screen, `IndependentFlip` =
//!   without composition.
//!
//! Needs the same rights as the stats island's `RecallFrameTiming`: an
//! elevated administrator or Performance Log Users ([`crate::frame_timing::can_trace`]).

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{w, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Diagnostics::Etw::{
    CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW, TdhGetProperty, CONTROLTRACE_HANDLE,
    ENABLE_TRACE_PARAMETERS, ENABLE_TRACE_PARAMETERS_VERSION_2, EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_FILTER_DESCRIPTOR,
    EVENT_FILTER_TYPE_EVENT_ID, EVENT_FILTER_TYPE_PID, EVENT_HEADER_FLAG_32_BIT_HEADER, EVENT_RECORD, EVENT_TRACE_CONTROL_QUERY, EVENT_TRACE_CONTROL_STOP,
    EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, PROCESSTRACE_HANDLE,
    PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_RAW_TIMESTAMP, PROCESS_TRACE_MODE_REAL_TIME, PROPERTY_DATA_DESCRIPTOR,
    TRACE_LEVEL_INFORMATION, WNODE_FLAG_TRACED_GUID,
};

use crate::render_verdict::RenderStats;

/// The ETW session's name, per Windows session: ETW names are machine-wide, and a second
/// signed-in user's controller stopping a "stale" session must not end this one's probe
/// (audit G3, R2-deep-startup-8).
const SESSION_BASE: &str = "RecallDetectRender";
/// `RecallDetectRender-<session>`, or the bare name when the session id can't be read.
pub(crate) fn session_name_for(session: Option<u32>) -> String {
    match session {
        Some(id) => format!("{SESSION_BASE}-{id}"),
        None => SESSION_BASE.to_string(),
    }
}

/// This session's name, NUL-terminated for the ETW calls (built once).
fn session_wide() -> &'static [u16] {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    NAME.get_or_init(|| session_name_for(crate::control_pipe::current_session()).encode_utf16().chain([0]).collect())
}

fn session() -> PCWSTR {
    PCWSTR(session_wide().as_ptr())
}
/// `EVENT_TRACE_USE_MS_FLUSH_TIMER`: `FlushTimer` is in milliseconds.
const MS_FLUSH_TIMER: u32 = 0x10;
/// Events reach the callback within this (the start debounce is 300 ms).
const FLUSH_MS: u32 = 50;
const INVALID_TRACE: u64 = u64::MAX;

const DXGI: GUID = GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);
const D3D9: GUID = GUID::from_u128(0x783ACA0A_790E_4D7F_8451_AA850511C6B9);
const DXGKRNL: GUID = GUID::from_u128(0x802EC45A_1E99_4B83_9920_87C98277BA9D);
const WIN32K: GUID = GUID::from_u128(0x8C416C79_D49B_4F01_A467_E56D3AA8234C);

const DXGI_PRESENT: u16 = 42;
const DXGI_PRESENT_MPO: u16 = 55;
const D3D9_PRESENT: u16 = 1;
const KRNL_PRESENT_INFO: u16 = 184;
const KRNL_FLIP_INFO: u16 = 168;
const W32K_SURFACE: u16 = 201;
const W32K_TOKEN_STATE: u16 = 301;

const DXGI_PRESENT_TEST: u32 = 0x1;
const DXGI_PRESENT_ALLOW_TEARING: u32 = 0x200;
/// `TokenState::InFrame`: composition of this frame starts.
const IN_FRAME: u32 = 3;

struct Provider {
    guid: GUID,
    any: u64,
    ids: &'static [u16],
    /// User-mode providers only: a PID filter would disable a kernel provider.
    pid_filter: bool,
}

const PROVIDERS: [Provider; 4] = [
    Provider { guid: DXGI, any: 0x8000_0000_0000_0002, ids: &[DXGI_PRESENT, DXGI_PRESENT_MPO], pid_filter: true },
    Provider { guid: D3D9, any: 0x8000_0000_0000_0002, ids: &[D3D9_PRESENT], pid_filter: true },
    // Present | Base, without the Performance keyword (PresentMon's PatchKeyword).
    Provider { guid: DXGKRNL, any: 0x0800_0001, ids: &[KRNL_FLIP_INFO, KRNL_PRESENT_INFO], pid_filter: false },
    // Updates: both token events carry it.
    Provider { guid: WIN32K, any: 0x1000, ids: &[W32K_SURFACE, W32K_TOKEN_STATE], pid_filter: false },
];

#[derive(Debug)]
pub enum StartError {
    /// Neither an elevated administrator nor in Performance Log Users.
    Denied,
    Failed(String),
}

#[repr(C)]
struct Properties {
    props: EVENT_TRACE_PROPERTIES,
    name: [u16; 64],
}

impl Properties {
    fn new() -> Box<Self> {
        let mut p = Box::new(Properties { props: EVENT_TRACE_PROPERTIES::default(), name: [0; 64] });
        p.props.Wnode.BufferSize = std::mem::size_of::<Properties>() as u32;
        p.props.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        p.props.Wnode.ClientContext = 1; // QPC
        p.props.LogFileMode = EVENT_TRACE_REAL_TIME_MODE | MS_FLUSH_TIMER;
        p.props.BufferSize = 16; // KB
        p.props.FlushTimer = FLUSH_MS;
        p.props.LoggerNameOffset = std::mem::offset_of!(Properties, name) as u32;
        p
    }
}

/// Stops this session's `RecallDetectRender-<session>` left by a killed controller. (A panic
/// on the pump thread unwinds through the controller, whose probe's drop
/// stops the session; only a killed process leaves one, for the next start.)
pub fn stop_stale() -> bool {
    let mut p = Properties::new();
    let status = unsafe { ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, session(), &mut p.props, EVENT_TRACE_CONTROL_STOP) };
    status == ERROR_SUCCESS
}

/// The machine-wide name every controller used before the per-session names. A controller of
/// that build killed mid-probe leaves it running until reboot, so a starting controller stops
/// it (merge-gate ruling, deep-startup-p3 c), and only this exact name: another Windows
/// session's `RecallDetectRender-<id>` is never touched.
pub(crate) const LEGACY_SESSION: &str = SESSION_BASE;

/// What [`stop_left_sessions`] stopped.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LeftSessions {
    /// This session's own name (a killed controller of this build).
    pub own: bool,
    /// The legacy machine-wide name (a killed controller of an older build).
    pub legacy: bool,
}

/// At controller start: stops this session's left-over session (`own`) and the legacy
/// machine-wide one through `stop` (the live path passes [`stop_named`]).
pub(crate) fn stop_left_sessions(own: &str, mut stop: impl FnMut(&str) -> bool) -> LeftSessions {
    let own_stopped = stop(own);
    // No session id: this session's name is the bare one, already stopped above.
    let legacy = own != LEGACY_SESSION && stop(LEGACY_SESSION);
    LeftSessions { own: own_stopped, legacy }
}

/// This Windows session's ETW session name.
pub(crate) fn own_session_name() -> String {
    session_name_for(crate::control_pipe::current_session())
}

/// Stops the ETW session called `name` (any process's). True when one was stopped.
pub(crate) fn stop_named(name: &str) -> bool {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let mut p = Properties::new();
    // SAFETY: `wide` is NUL-terminated and outlives the call; `p` is a sized properties block.
    let status = unsafe { ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, PCWSTR(wide.as_ptr()), &mut p.props, EVENT_TRACE_CONTROL_STOP) };
    status == ERROR_SUCCESS
}

/// This Windows session's `RecallDetectRender-<session>` exists (any process's).
pub fn session_running() -> bool {
    let mut p = Properties::new();
    let status = unsafe { ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, session(), &mut p.props, EVENT_TRACE_CONTROL_QUERY) };
    status == ERROR_SUCCESS
}

#[derive(Default)]
struct Tally {
    stats: RenderStats,
    /// The target's composition surfaces (`pCompositionSurfaceObject`).
    surfaces: Vec<u64>,
}

struct Shared {
    pid: AtomicU32,
    tally: Mutex<Tally>,
}

/// One watch of one process. Dropping it stops the session.
pub struct RenderProbe {
    control: CONTROLTRACE_HANDLE,
    trace: Option<PROCESSTRACE_HANDLE>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    pub pid: u32,
    pub since: Instant,
    /// StartTrace -> consumer running.
    pub start_cost: Duration,
}

impl RenderProbe {
    pub fn start(pid: u32) -> Result<Self, StartError> {
        let t0 = Instant::now();
        stop_stale();
        let mut p = Properties::new();
        let mut control = CONTROLTRACE_HANDLE { Value: 0 };
        match unsafe { StartTraceW(&mut control, session(), &mut p.props) } {
            ERROR_SUCCESS => {}
            ERROR_ACCESS_DENIED => return Err(StartError::Denied),
            other => return Err(StartError::Failed(format!("StartTrace {}: error {}", session_name_for(crate::control_pipe::current_session()), other.0))),
        }
        let shared = Arc::new(Shared { pid: AtomicU32::new(pid), tally: Mutex::new(Tally::default()) });
        // From here on, dropping `probe` stops the session on any error.
        let mut probe = RenderProbe {
            control,
            trace: None,
            thread: None,
            shared,
            pid,
            since: t0,
            start_cost: Duration::ZERO,
        };
        probe.enable_all(pid, p.props.Wnode.Guid)?;
        probe.consume()?;
        probe.since = Instant::now();
        probe.start_cost = probe.since - t0;
        Ok(probe)
    }

    pub fn stats(&self) -> RenderStats {
        self.shared.tally.lock().unwrap_or_else(PoisonError::into_inner).stats
    }

    fn enable_all(&self, pid: u32, source: GUID) -> Result<(), StartError> {
        let mut enabled = 0;
        for spec in &PROVIDERS {
            let mut status = enable(self.control, source, spec, spec.pid_filter.then_some(pid));
            if status != ERROR_SUCCESS && status != ERROR_ACCESS_DENIED && spec.pid_filter {
                status = enable(self.control, source, spec, None); // the callback filters too
            }
            match status {
                ERROR_SUCCESS => enabled += 1,
                ERROR_ACCESS_DENIED => return Err(StartError::Denied),
                other => crate::log!("record: render check: provider {:?} not enabled (error {})", spec.guid, other.0),
            }
        }
        if enabled == 0 {
            return Err(StartError::Failed("no provider enabled".into()));
        }
        Ok(())
    }

    fn consume(&mut self) -> Result<(), StartError> {
        let mut name: Vec<u16> = session_wide().to_vec();
        let mut logfile = EVENT_TRACE_LOGFILEW { LoggerName: PWSTR(name.as_mut_ptr()), ..Default::default() };
        logfile.Anonymous1.ProcessTraceMode =
            PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
        logfile.Anonymous2.EventRecordCallback = Some(on_event);
        logfile.Context = Arc::as_ptr(&self.shared) as *mut c_void;
        let trace = unsafe { OpenTraceW(&mut logfile) };
        if trace.Value == INVALID_TRACE {
            return Err(StartError::Failed(format!("OpenTrace: {}", std::io::Error::last_os_error())));
        }
        self.trace = Some(trace);
        let shared = Arc::clone(&self.shared); // keeps the callback's context alive
        let thread = std::thread::Builder::new()
            .name("recall-detect-render".into())
            .spawn(move || {
                let _ = unsafe { ProcessTrace(&[trace], None, None) };
                drop(shared);
            })
            .map_err(|e| StartError::Failed(format!("consumer thread: {e}")))?;
        self.thread = Some(thread);
        Ok(())
    }
}

impl Drop for RenderProbe {
    fn drop(&mut self) {
        let mut p = Properties::new();
        unsafe {
            let _ = ControlTraceW(self.control, PCWSTR::null(), &mut p.props, EVENT_TRACE_CONTROL_STOP);
            if let Some(trace) = self.trace.take() {
                let _ = CloseTrace(trace);
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn enable(control: CONTROLTRACE_HANDLE, source: GUID, spec: &Provider, pid: Option<u32>) -> WIN32_ERROR {
    // EVENT_FILTER_EVENT_ID: FilterIn = TRUE, Reserved = 0, Count, Events[].
    let mut ids: Vec<u16> = vec![1, spec.ids.len() as u16];
    ids.extend_from_slice(spec.ids);
    let pids = [pid.unwrap_or(0)];
    let mut filters = vec![EVENT_FILTER_DESCRIPTOR { Ptr: ids.as_ptr() as u64, Size: (ids.len() * 2) as u32, Type: EVENT_FILTER_TYPE_EVENT_ID }];
    if pid.is_some() {
        filters.push(EVENT_FILTER_DESCRIPTOR { Ptr: pids.as_ptr() as u64, Size: 4, Type: EVENT_FILTER_TYPE_PID });
    }
    let params = ENABLE_TRACE_PARAMETERS {
        Version: ENABLE_TRACE_PARAMETERS_VERSION_2,
        EnableProperty: 0,
        ControlFlags: 0,
        SourceId: source,
        EnableFilterDesc: filters.as_mut_ptr(),
        FilterDescCount: filters.len() as u32,
    };
    let code = EVENT_CONTROL_CODE_ENABLE_PROVIDER.0;
    let level = TRACE_LEVEL_INFORMATION as u8;
    unsafe { EnableTraceEx2(control, &spec.guid, code, level, spec.any, 0, 0, Some(&params)) }
}

/// Runs on the consumer thread for every event. Must not panic.
unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    let Some(record) = (unsafe { record.as_ref() }) else { return };
    let Some(shared) = (unsafe { (record.UserContext as *const Shared).as_ref() }) else { return };
    let header = &record.EventHeader;
    let target = shared.pid.load(Ordering::Relaxed);
    let mine = header.ProcessId == target;
    let id = header.EventDescriptor.Id;
    let provider = header.ProviderId;
    let payload: &[u8] = if record.UserData.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(record.UserData as *const u8, usize::from(record.UserDataLength)) }
    };
    let ptr = if u32::from(header.Flags) & EVENT_HEADER_FLAG_32_BIT_HEADER != 0 { 4 } else { 8 };
    let mut tally = shared.tally.lock().unwrap_or_else(PoisonError::into_inner);
    match (provider, id) {
        (DXGI, DXGI_PRESENT | DXGI_PRESENT_MPO) if mine => {
            let flags = read_uint(payload, ptr, 4).unwrap_or(0) as u32;
            if flags & DXGI_PRESENT_TEST != 0 {
                return;
            }
            tally.stats.runtime_presents += 1;
            tally.stats.video_presents += u32::from(id == DXGI_PRESENT_MPO);
            tally.stats.tearing_presents += u32::from(flags & DXGI_PRESENT_ALLOW_TEARING != 0);
        }
        (D3D9, D3D9_PRESENT) if mine => tally.stats.runtime_presents += 1,
        (DXGKRNL, KRNL_PRESENT_INFO) if mine => tally.stats.kernel_presents += 1,
        (DXGKRNL, KRNL_FLIP_INFO) if mine => tally.stats.legacy_flips += 1,
        (WIN32K, W32K_SURFACE) if mine => {
            // pToken, then pCompositionSurfaceObject.
            if let Some(surface) = read_uint(payload, ptr, ptr) {
                if !tally.surfaces.contains(&surface) && tally.surfaces.len() < 16 {
                    tally.surfaces.push(surface);
                }
            }
        }
        (WIN32K, W32K_TOKEN_STATE) => {
            // Logged by DWM: matched to the target by its surface (the first field).
            let Some(surface) = read_uint(payload, 0, ptr) else { return };
            if !tally.surfaces.contains(&surface) || tdh_u32(record, w!("NewState")) != Some(IN_FRAME) {
                return;
            }
            match tdh_u32(record, w!("IndependentFlip")) {
                Some(0) => tally.stats.composed_frames += 1,
                Some(_) => tally.stats.independent_frames += 1,
                None => {}
            }
        }
        _ => {}
    }
}

/// A little-endian unsigned field of `size` bytes at `at`.
fn read_uint(payload: &[u8], at: usize, size: usize) -> Option<u64> {
    let bytes = payload.get(at..at.checked_add(size)?)?;
    let mut buf = [0u8; 8];
    buf.get_mut(..size)?.copy_from_slice(bytes);
    Some(u64::from_le_bytes(buf))
}

/// A 4-byte property by name (decoded through the event's manifest).
fn tdh_u32(record: &EVENT_RECORD, name: PCWSTR) -> Option<u32> {
    let desc = [PROPERTY_DATA_DESCRIPTOR { PropertyName: name.as_ptr() as u64, ArrayIndex: u32::MAX, Reserved: 0 }];
    let mut buf = [0u8; 4];
    let status = unsafe { TdhGetProperty(record, None, &desc, &mut buf) };
    (status == 0).then(|| u32::from_le_bytes(buf))
}

#[cfg(test)]
#[path = "render_probe_tests.rs"]
mod tests;
