//! The Record picker (maintainer, 2026-11-28: "when i have 3 monitors, i like to be specific"):
//! with 2 or more monitors, Record asks what to capture. The app shows the picker
//! (`recall-native` `views::record_picker`) and sends what was chosen:
//!
//! - `{"cmd":"record_pick","window":<hwnd>}`: pin that window, as `RECALL_CAPTURE_HWND` does;
//! - `{"cmd":"record_pick","monitor":"\\\\.\\DISPLAY2"}` (JSON-escaped `\\.\DISPLAY2`): capture that monitor
//!   ([`EngineTarget::Screen`](crate::record_engine::EngineTarget::Screen)).
//!
//! The controller retargets the engine (a game's replay is saved first, so the switch loses
//! nothing; the drain linger is skipped), then starts the long recording once the engine runs
//! on the pick ([`PickRun::start_by`]). The pick holds for the whole recording, "discord like
//! logic": a picked window that closes or minimises does not end it (the picture holds, the
//! sound goes on), a monitor keeps going. Stopping the recording releases the pick and
//! auto-detection resumes.
//!
//! The Record shortcut opens the picker only when no game is followed (maintainer: "desktopm has
//! less urgency"): a followed game records at once, with nothing popping up over it.

use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::core::BOOL;

use crate::log;
use crate::record_app::Controller;
use crate::record_target::{display_name, Target};

/// How long a pick may take to reach a running engine on its target before the start is given
/// up (a retarget waits for a replay save, then a new engine says hello).
pub const PICK_START_WAIT: Duration = Duration::from_secs(20);

/// The session event that asks the app to show the picker.
pub const PICKER_EVENT: &str = r#"{"type":"record_picker"}"#;

/// What the picker chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickRequest {
    Window(isize),
    Monitor(String),
}

/// Reads a `record_pick` line: exactly one of `window` (a non-zero handle) or `monitor` (a
/// device name).
pub fn parse_request(val: &Value) -> Result<PickRequest, &'static str> {
    let window = val.get("window").and_then(Value::as_u64).filter(|h| *h != 0);
    let monitor = val.get("monitor").and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty());
    match (window, monitor) {
        (Some(hwnd), None) => Ok(PickRequest::Window(hwnd as isize)),
        (None, Some(device)) => Ok(PickRequest::Monitor(device.to_string())),
        _ => Err("record_pick needs \"window\": <handle> or \"monitor\": <device name>"),
    }
}

/// A pick in progress: its start still owed, or its recording running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickRun {
    /// For log lines and refusals ("Desktop", the window's name).
    pub label: String,
    /// `record_start` is owed until this deadline.
    pub start_by: Option<Instant>,
    /// The engine hello count at the pick; a retarget waits for a newer hello.
    pub hello_at_pick: u32,
    /// The engine must restart onto the pick first.
    pub needs_hello: bool,
    /// The long recording started under this pick (its stop releases it).
    pub recording: bool,
    /// The pick set `Controller::pinned` (released with it; an env pin is never touched).
    pub pinned: bool,
    /// The picked window or its process is gone (the recording holds on; read no more).
    pub gone: bool,
}

/// What [`step`] wants done this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickStep {
    Wait,
    /// Send `record_start` now.
    Start,
    /// The start ran out of time: refuse it and release the pick.
    GiveUp,
    /// The pick's recording ended: release the pick.
    Release,
}

/// One tick of a pick. `on_target`: the engine runs and was applied the wanted target;
/// `hellos`: the engine hello count now; `recording`: a long recording runs or starts.
pub fn step(run: &PickRun, now: Instant, on_target: bool, hellos: u32, recording: bool) -> PickStep {
    if let Some(by) = run.start_by {
        let fresh = !run.needs_hello || hellos > run.hello_at_pick;
        if on_target && fresh {
            return PickStep::Start;
        }
        return if now >= by { PickStep::GiveUp } else { PickStep::Wait };
    }
    if run.recording && !recording {
        return PickStep::Release;
    }
    PickStep::Wait
}

/// Whether the Record shortcut opens the picker instead of toggling: the setting is on, two
/// or more monitors, no recording runs or starts, and no game (or pinned window) is followed.
pub fn shortcut_opens_picker(ask: bool, monitors: usize, recording: bool, following: bool) -> bool {
    ask && monitors >= 2 && !recording && !following
}

/// The picked screen is no longer connected (unplugged, asleep, renumbered): its recording
/// stops and is saved; it never moves to a screen nobody picked.
pub fn picked_screen_gone(picked: Option<&str>, connected: &[String]) -> bool {
    picked.is_some_and(|p| !connected.iter().any(|d| d.eq_ignore_ascii_case(p)))
}

/// Every monitor's GDI device name (`\\.\DISPLAY1`), in enumeration order.
pub fn monitor_devices() -> Vec<String> {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
        // SAFETY: `out` is the Vec below, alive for the whole EnumDisplayMonitors call.
        let list = unsafe { &mut *(out.0 as *mut Vec<String>) };
        let mut info = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, ..Default::default() };
        // SAFETY: a sized MONITORINFOEXW for a handle the enumeration passed in.
        if unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
            let end = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
            list.push(String::from_utf16_lossy(&info.szDevice[..end]));
        }
        BOOL(1)
    }
    let mut list: Vec<String> = Vec::new();
    // SAFETY: the callback only pushes to `list`, which outlives the call.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut list as *mut Vec<String> as isize)) };
    list
}

impl Controller {
    /// `record_pick` from the app's picker.
    pub(crate) fn record_pick(&mut self, val: &Value, client: Option<u64>) {
        let request = match parse_request(val) {
            Ok(r) => r,
            Err(e) => return self.reply_error(client, e),
        };
        if self.paused {
            return self.refuse_capture("record_start", crate::record_engine::PAUSED_REASON);
        }
        if self.activity.recording_or_starting() || self.pick.is_some() {
            return self.reply_error(client, "record_pick: a recording is already running or starting");
        }
        let (pinned, screen, label) = match request {
            PickRequest::Window(hwnd) => match self.pick_window(hwnd) {
                Some(t) => {
                    let label = t.name.clone();
                    (Some(t), None, label)
                }
                None => return self.refuse_capture("record_start", "that window is gone"),
            },
            PickRequest::Monitor(device) => {
                if !monitor_devices().iter().any(|d| d.eq_ignore_ascii_case(&device)) {
                    return self.refuse_capture("record_start", "that monitor is not connected");
                }
                (None, Some(device), crate::record_target::DESKTOP_NAME.to_string())
            }
        };
        let now = Instant::now();
        let before = self.planner.applied().clone();
        let after = crate::record_engine::wanted_with(None, pinned.as_ref(), screen.as_deref(), &self.cfg.recording, false);
        let moves = before != after;
        if moves && matches!(before, crate::record_engine::EngineTarget::Window(_)) && self.engine_running() {
            // Leaving a game: keep what its replay holds, then skip the drain (the switch waits
            // for this save: `Step::Held`).
            log!("record: Record picker leaves {}; saving its replay first", target_label(&before));
            self.save_clip(None);
            self.planner.skip_linger(now);
        }
        log!("record: Record picker chose {label}{}", if moves { " (retarget)" } else { "" });
        recall_trace::act("record.pick", &[("kind", if screen.is_some() { "screen" } else { "window" }.into())]);
        let pinned_by_pick = pinned.is_some();
        if pinned_by_pick {
            self.pinned = pinned;
        }
        self.picked_screen = screen;
        self.pick = Some(PickRun {
            label,
            start_by: Some(now + PICK_START_WAIT),
            hello_at_pick: self.supervisor.hello_count(),
            needs_hello: moves,
            recording: false,
            pinned: pinned_by_pick,
            gone: false,
        });
        self.pick_tick(now);
    }

    /// The picked window as a target: the followed game's own entry when it is that window
    /// (its game name), else the window's title.
    fn pick_window(&self, hwnd: isize) -> Option<Target> {
        if let Some(t) = self.tracker.current().filter(|t| t.hwnd == hwnd) {
            return Some(t.clone());
        }
        let (pid, stem, exe, title) = crate::watcher::window_identity(hwnd)?;
        Some(Target { hwnd, pid, name: display_name(&title, &stem), exe })
    }

    /// Each tick: start the pick's recording once the engine runs on it, give up after
    /// [`PICK_START_WAIT`], release the pick when its recording ends.
    pub(crate) fn pick_tick(&mut self, now: Instant) {
        let Some(run) = self.pick.as_ref() else { return };
        let on_target = self.engine_running() && *self.planner.applied() == self.wanted();
        let recording = self.activity.recording_or_starting();
        match step(run, now, on_target, self.supervisor.hello_count(), recording) {
            PickStep::Wait => {}
            PickStep::Start => {
                let label = run.label.clone();
                if self.engine_command_queued("record_start") {
                    log!("record: Record picker recording {label}");
                    if let Some(run) = self.pick.as_mut() {
                        run.start_by = None;
                        run.recording = true;
                    }
                } else {
                    self.release_pick("the start was refused");
                }
            }
            PickStep::GiveUp => {
                let reason = format!("the recorder did not start on {} in time", run.label);
                self.refuse_capture("record_start", &reason);
                self.release_pick("start timed out");
            }
            PickStep::Release => self.release_pick("its recording ended"),
        }
    }

    /// The displays changed: a picked screen that is gone stops and saves its recording.
    pub(crate) fn check_picked_screen(&mut self) {
        if !picked_screen_gone(self.picked_screen.as_deref(), &monitor_devices()) {
            return;
        }
        log!("record: the picked screen {} is gone; stopping and saving", self.picked_screen.as_deref().unwrap_or("?"));
        if self.activity.recording_or_starting() {
            let _ = self.engine_command_queued("record_stop");
        }
        let notice = crate::record_notice::shortcut_notice_json("The screen you picked is gone", "The recording was saved.", false, true);
        self.hub.event("session", &notice);
        self.release_pick("the picked screen is gone");
    }

    /// The pick is over: auto-detection resumes (an env pin stays).
    pub(crate) fn release_pick(&mut self, why: &str) {
        let Some(run) = self.pick.take() else { return };
        log!("record: Record picker released ({why})");
        if run.pinned {
            self.pinned = None;
        }
        self.picked_screen = None;
    }

    /// A picked window's recording holds it even after the window or its process is gone.
    pub(crate) fn pick_holds_window(&self) -> bool {
        self.pick.as_ref().is_some_and(|r| r.pinned && (r.recording || r.start_by.is_some()))
    }

    /// The picked window is known gone: its liveness is read no more.
    pub(crate) fn pick_window_gone(&self) -> bool {
        self.pick.as_ref().is_some_and(|r| r.pinned && r.gone)
    }

    /// The picked window closed or its process exited: the recording goes on until stopped.
    pub(crate) fn note_pick_window_gone(&mut self) {
        if let Some(run) = self.pick.as_mut().filter(|r| !r.gone) {
            run.gone = true;
            log!("record: picked window {} is gone; the recording goes on until stopped", run.label);
        }
    }

    /// The Record shortcut: the picker when [`shortcut_opens_picker`], else `false` (toggle).
    pub(crate) fn shortcut_picker(&mut self) -> bool {
        // Never over a fullscreen app Recall does not follow: its window would take focus from it.
        if crate::record_fullscreen::exclusive_now() || crate::watcher::foreground_covers_screen() {
            log!("record: Record shortcut over a fullscreen window: no picker");
            return false;
        }
        let following = self.tracker.current().is_some() || self.pinned.is_some();
        let recording = self.activity.recording_or_starting() || self.pick.is_some();
        if !shortcut_opens_picker(self.cfg.recording.ask_record_target, monitor_devices().len(), recording, following) {
            return false;
        }
        log!("record: Record shortcut with no game -> the app's Record picker");
        self.hub.event("session", PICKER_EVENT);
        self.ui.open_with_picker();
        true
    }
}

fn target_label(target: &crate::record_engine::EngineTarget) -> String {
    match target {
        crate::record_engine::EngineTarget::Window(t) => t.name.clone(),
        _ => crate::record_target::DESKTOP_NAME.to_string(),
    }
}

#[cfg(test)]
#[path = "record_pick_tests.rs"]
mod tests;
