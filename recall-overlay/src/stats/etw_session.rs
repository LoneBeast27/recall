//! The real-time ETW session `RecallFrameTiming`: Windows' own present events
//! for the target process (the method Intel PresentMon and NVIDIA FrameView
//! use). Nothing is injected into the game: the D3D runtime and the graphics
//! kernel already log these events; the session only subscribes.
//!
//! Starting a session needs an elevated administrator or membership in
//! Performance Log Users (S-1-5-32-559); otherwise `StartTrace` returns
//! `ERROR_ACCESS_DENIED` ([`StartError::Denied`]). Session setup follows
//! PresentMon's `PMTraceSession::Start` (MIT): QPC clock, real-time mode, raw
//! timestamps, event-id filters per provider (`present_events::PROVIDERS`),
//! plus a PID filter on the user-mode providers. The callback filters on the
//! target pid as well, so correctness never depends on a filter.
//!
//! Stopped on drop (stats off, overlay exit), by the panic hook / console
//! handler ([`install_cleanup`]), by the next start that finds a stale session
//! of the same name (a killed overlay or probe) and by the controller after it
//! kills the overlay.
//!
//! Names ([`session_name`]): the island's `RecallFrameTiming`, the
//! `--frame-timing-probe`'s `RecallFrameTimingProbe`, each with `.<tag>` when
//! `RECALL_INSTANCE_TAG` is set (tests and the build gate), so a probe or a
//! tagged overlay never touches the installed overlay's live session.
//!
//! ETW session names are machine-wide while the one-island guard is per user
//! (`Local\`), so ownership is its own lock: `Global\<session name>.owner`,
//! held for the session's life ([`owner_lock`]). An existing session is
//! stopped and replaced only when that lock is free (its owner is dead). A
//! held lock, including another user's (access denied), means a live owner:
//! the start fails and is retried later.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use windows::core::{BOOL, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Diagnostics::Etw::{
    CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE,
    ENABLE_TRACE_PARAMETERS, ENABLE_TRACE_PARAMETERS_VERSION_2, EVENT_CONTROL_CODE_DISABLE_PROVIDER,
    EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_ENABLE_PROPERTY_IGNORE_KEYWORD_0, EVENT_FILTER_DESCRIPTOR,
    EVENT_FILTER_TYPE_EVENT_ID, EVENT_FILTER_TYPE_PID, EVENT_HEADER_FLAG_32_BIT_HEADER, EVENT_RECORD,
    EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE,
    PROCESSTRACE_HANDLE, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_RAW_TIMESTAMP,
    PROCESS_TRACE_MODE_REAL_TIME, TRACE_LEVEL_INFORMATION, WNODE_FLAG_TRACED_GUID,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use super::frame_math::{FrameStats, FrameWindow};
use super::enable_plan::{self, Aimed, Enabler};
use super::present_events::{classify, FrameMapper, Mapped, ProviderSpec, RawEvent, PROVIDERS};

use crate::single_instance::{self, Claim, Guard};

/// Whose session this is (its name).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The island's stats (`RecallFrameTiming`; also named in
    /// `recall-hdr/src/frame_timing.rs`, which stops it after killing the
    /// overlay).
    Overlay,
    /// `--frame-timing-probe` (`RecallFrameTimingProbe`).
    Probe,
}

/// The session name for `kind` and the (cleaned) instance tag; the tag is
/// cut to 32 characters so the name fits the 64-character buffer.
pub fn session_name(kind: Kind, tag: &str) -> String {
    let base = match kind {
        Kind::Overlay => "RecallFrameTiming",
        Kind::Probe => "RecallFrameTimingProbe",
    };
    let tag: String = tag.chars().take(32).collect();
    if tag.is_empty() {
        base.to_owned()
    } else {
        format!("{base}.{tag}")
    }
}

/// The lock a live owner of session `name` holds (machine-wide).
pub fn owner_lock(name: &str) -> String {
    format!("Global\\{name}.owner")
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// `Wnode.ClientContext` = 1: query performance counter timestamps.
const CLOCK_QPC: u32 = 1;
/// `INVALID_PROCESSTRACE_HANDLE` on 64-bit Windows.
const INVALID_TRACE: u64 = u64::MAX;

/// The name of this process's running session (the panic hook and console
/// handler stop it by name only then).
static OWNED: Mutex<Option<String>> = Mutex::new(None);

fn owned() -> MutexGuard<'static, Option<String>> {
    OWNED.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
pub enum StartError {
    /// Not an elevated administrator and not in Performance Log Users.
    Denied,
    /// The session name is another live process's (another Recall, perhaps
    /// another signed-in user's), or its owner lock could not be made while a
    /// session of that name runs: retried later, and the island says so.
    Owned(String),
    Failed(String),
}

/// `EVENT_TRACE_PROPERTIES` followed by room for the session name.
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
        p.props.Wnode.ClientContext = CLOCK_QPC;
        p.props.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        // Small events at a few hundred per second: 16 KB buffers, ETW picks
        // the count (2 per CPU); flushed every second for the live display.
        p.props.BufferSize = 16;
        p.props.FlushTimer = 1;
        p.props.LoggerNameOffset = std::mem::offset_of!(Properties, name) as u32;
        p
    }
}

/// Stops the session `name` (any process's); `ERROR_SUCCESS` or the reason
/// (4201 = none running, 5 = no permission).
fn stop_by_name(name: &str) -> WIN32_ERROR {
    let mut p = Properties::new();
    let name = wide(name);
    unsafe {
        ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, PCWSTR(name.as_ptr()), &mut p.props, EVENT_TRACE_CONTROL_STOP)
    }
}

/// `StartTraceW` for the NUL-terminated `name`.
fn start_trace(name: &[u16], p: &mut Properties, control: &mut CONTROLTRACE_HANDLE) -> WIN32_ERROR {
    unsafe { StartTraceW(control, PCWSTR(name.as_ptr()), &mut p.props) }
}

/// A panic or a console close/ctrl event still stops our session.
pub fn install_cleanup() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        stop_if_owned();
        previous(info);
    }));
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(on_console_ctrl), true);
    }
}

fn stop_if_owned() {
    if let Some(name) = owned().take() {
        let _ = stop_by_name(&name);
    }
}

unsafe extern "system" fn on_console_ctrl(_ctrl: u32) -> BOOL {
    stop_if_owned();
    BOOL(0) // let the default handling end the process
}

struct Track {
    mapper: FrameMapper,
    window: FrameWindow,
}

/// What the ETW callback thread and the overlay share.
struct Shared {
    target: AtomicU32,
    running: AtomicBool,
    ticks_per_sec: i64,
    track: Mutex<Track>,
}

impl Shared {
    fn track(&self) -> MutexGuard<'_, Track> {
        self.track.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn retarget(&self, pid: u32) {
        self.target.store(pid, Ordering::SeqCst);
        let mut track = self.track();
        track.mapper = FrameMapper::new(pid, self.ticks_per_sec);
        track.window.clear();
    }
}

pub struct EtwSession {
    name: String,
    /// `Global\<name>.owner`, released after the trace stops (fields drop
    /// after `Drop::drop`). `None`: the lock could not be made at all.
    _owner: Option<Guard>,
    control: CONTROLTRACE_HANDLE,
    guid: GUID,
    trace: Option<PROCESSTRACE_HANDLE>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    /// How the providers run for the current target (`enable_plan`).
    aimed: Aimed,
    /// `RECALL_ETW_NO_PID_FILTER=1` (dev: filtered vs unfiltered CPU A/B).
    forced_off: bool,
    /// Enable problems not yet written to the trace log ([`Self::take_problems`]).
    problems: Vec<String>,
}

/// `RECALL_ETW_NO_PID_FILTER=1`: never PID-filter (dev A/B only).
fn pid_filter_forced_off() -> bool {
    std::env::var("RECALL_ETW_NO_PID_FILTER").as_deref() == Ok("1")
}

/// The real provider calls for `enable_plan`.
struct Calls<'a>(&'a EtwSession);

impl Enabler for Calls<'_> {
    fn enable(&mut self, spec: &ProviderSpec, pid: Option<u32>) -> WIN32_ERROR {
        self.0.enable(spec, pid)
    }

    fn disable(&mut self, spec: &ProviderSpec) {
        self.0.disable(spec);
    }
}

impl EtwSession {
    /// Starts `kind`'s session for `pid`, named with this process's
    /// `RECALL_INSTANCE_TAG`.
    pub fn start(pid: u32, kind: Kind) -> Result<Self, StartError> {
        Self::start_named(pid, session_name(kind, &single_instance::tag()))
    }

    fn start_named(pid: u32, name: String) -> Result<Self, StartError> {
        let ticks_per_sec = qpc_frequency();
        let owner = match single_instance::claim(&owner_lock(&name)) {
            Claim::First(guard) => Some(guard),
            Claim::Taken => return Err(StartError::Owned(format!("{name} has a live owner"))),
            Claim::Unavailable(_) => None, // no lock: never stop an existing session
        };
        let wide_name = wide(&name);
        let mut p = Properties::new();
        let mut control = CONTROLTRACE_HANDLE { Value: 0 };
        let mut status = start_trace(&wide_name, &mut p, &mut control);
        if status == ERROR_ALREADY_EXISTS && owner.is_some() {
            let _ = stop_by_name(&name); // its owner is dead: a killed overlay's or probe's
            p = Properties::new();
            status = start_trace(&wide_name, &mut p, &mut control);
        }
        match status {
            ERROR_SUCCESS => {}
            ERROR_ACCESS_DENIED => return Err(StartError::Denied),
            ERROR_ALREADY_EXISTS => {
                return Err(StartError::Owned(format!("{name} already runs (owner lock unavailable)")));
            }
            other => return Err(StartError::Failed(format!("StartTrace {name}: error {}", other.0))),
        }
        *owned() = Some(name.clone());
        let shared = Arc::new(Shared {
            target: AtomicU32::new(pid),
            running: AtomicBool::new(false),
            ticks_per_sec,
            track: Mutex::new(Track { mapper: FrameMapper::new(pid, ticks_per_sec), window: FrameWindow::new(ticks_per_sec) }),
        });
        // From here on, dropping `session` stops the trace on any error.
        let forced_off = pid_filter_forced_off();
        let mut session = EtwSession {
            name,
            _owner: owner,
            control,
            guid: p.props.Wnode.Guid,
            trace: None,
            thread: None,
            shared,
            aimed: Aimed::new(!forced_off),
            forced_off,
            problems: Vec::new(),
        };
        session.enable_providers(pid)?;
        session.consume()?;
        Ok(session)
    }

    /// Follow another process: new filter (asked filtered again, see
    /// `enable_plan::retarget`), fresh numbers.
    pub fn retarget(&mut self, pid: u32) {
        self.shared.retarget(pid);
        let providers = PROVIDERS;
        let specs: Vec<&ProviderSpec> = providers.iter().collect();
        let aimed = enable_plan::retarget(&specs, pid, self.aimed.filter_ok, &mut Calls(self));
        if let Some(aimed) = aimed {
            self.problems.extend(enable_plan::problems(&aimed, pid));
            self.aimed = Aimed { kernel: self.aimed.kernel, ..aimed };
        }
    }

    /// Enable problems since the last call (lost filter, providers that did
    /// not come up), for the trace log.
    pub fn take_problems(&mut self) -> Vec<String> {
        std::mem::take(&mut self.problems)
    }

    /// The trace note's tail: empty while PID-filtered, else why not.
    pub fn filter_note(&self) -> String {
        enable_plan::filter_note(&self.aimed, self.forced_off)
    }

    pub fn stats(&self) -> FrameStats {
        self.shared.track().window.stats(qpc_now())
    }

    /// The consumer thread is still delivering events.
    pub fn alive(&self) -> bool {
        self.shared.running.load(Ordering::SeqCst)
    }

    /// DXGI (every D3D10/11/12 game) or the DxgKrnl fallback (any API) must
    /// come up; D3D9 is best effort. Filter fallbacks: `enable_plan`.
    fn enable_providers(&mut self, pid: u32) -> Result<(), StartError> {
        let providers = PROVIDERS;
        let specs: Vec<&ProviderSpec> = providers.iter().collect();
        let aimed = enable_plan::enable_for(&specs, pid, self.aimed.filter_ok, &mut Calls(self));
        self.problems.extend(enable_plan::problems(&aimed, pid));
        self.aimed = aimed;
        enable_plan::outcome(&self.aimed)
    }

    fn enable(&self, spec: &ProviderSpec, pid: Option<u32>) -> WIN32_ERROR {
        // EVENT_FILTER_EVENT_ID: FilterIn = TRUE, Reserved = 0, Count, Events[].
        let mut ids: Vec<u16> = vec![1, spec.event_ids.len() as u16];
        ids.extend_from_slice(spec.event_ids);
        let pids = [pid.unwrap_or(0)];
        let mut filters = vec![EVENT_FILTER_DESCRIPTOR {
            Ptr: ids.as_ptr() as u64,
            Size: (ids.len() * 2) as u32,
            Type: EVENT_FILTER_TYPE_EVENT_ID,
        }];
        if pid.is_some() {
            filters.push(EVENT_FILTER_DESCRIPTOR { Ptr: pids.as_ptr() as u64, Size: 4, Type: EVENT_FILTER_TYPE_PID });
        }
        let params = ENABLE_TRACE_PARAMETERS {
            Version: ENABLE_TRACE_PARAMETERS_VERSION_2,
            EnableProperty: EVENT_ENABLE_PROPERTY_IGNORE_KEYWORD_0,
            ControlFlags: 0,
            SourceId: self.guid,
            EnableFilterDesc: filters.as_mut_ptr(),
            FilterDescCount: filters.len() as u32,
        };
        let level = TRACE_LEVEL_INFORMATION as u8;
        let code = EVENT_CONTROL_CODE_ENABLE_PROVIDER.0;
        unsafe { EnableTraceEx2(self.control, &spec.guid, code, level, spec.keyword, spec.keyword, 0, Some(&params)) }
    }

    fn disable(&self, spec: &ProviderSpec) {
        let code = EVENT_CONTROL_CODE_DISABLE_PROVIDER.0;
        let _ = unsafe { EnableTraceEx2(self.control, &spec.guid, code, 0, 0, 0, 0, None) };
    }

    /// Opens the real-time consumer and runs `ProcessTrace` on its own thread.
    fn consume(&mut self) -> Result<(), StartError> {
        let mut name = wide(&self.name);
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
        self.shared.running.store(true, Ordering::SeqCst);
        let shared = Arc::clone(&self.shared); // keeps the callback's context alive
        let thread = std::thread::Builder::new()
            .name("recall-frame-timing".into())
            .spawn(move || {
                let _ = unsafe { ProcessTrace(&[trace], None, None) };
                shared.running.store(false, Ordering::SeqCst);
            })
            .map_err(|e| StartError::Failed(format!("consumer thread: {e}")))?;
        self.thread = Some(thread);
        Ok(())
    }
}

impl Drop for EtwSession {
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
        *owned() = None;
    }
}

/// `EventRecordCallback`: runs on the consumer thread for every event.
/// Must not panic (it is called from Windows).
unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    let Some(record) = (unsafe { record.as_ref() }) else { return };
    let Some(shared) = (unsafe { (record.UserContext as *const Shared).as_ref() }) else { return };
    let header = &record.EventHeader;
    if header.ProcessId != shared.target.load(Ordering::Relaxed) {
        return; // cheapest first: most events are other processes' when unfiltered
    }
    let payload: &[u8] = if record.UserData.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(record.UserData as *const u8, usize::from(record.UserDataLength)) }
    };
    let pointer_size = if u32::from(header.Flags) & EVENT_HEADER_FLAG_32_BIT_HEADER != 0 { 4 } else { 8 };
    let event = RawEvent { provider: header.ProviderId, id: header.EventDescriptor.Id, pointer_size, payload };
    let Some(source) = classify(&event) else { return };
    let mut track = shared.track();
    match track.mapper.map(header.ProcessId, source, header.TimeStamp) {
        Mapped::Ignore => {}
        Mapped::Frame(at) => track.window.push(at),
        Mapped::Restart(at) => {
            track.window.clear();
            track.window.push(at);
        }
    }
}

fn qpc_frequency() -> i64 {
    let mut hz = 0i64;
    let _ = unsafe { QueryPerformanceFrequency(&mut hz) };
    hz.max(1)
}

fn qpc_now() -> i64 {
    let mut now = 0i64;
    let _ = unsafe { QueryPerformanceCounter(&mut now) };
    now
}

#[cfg(test)]
#[path = "etw_session_tests.rs"]
mod tests;
