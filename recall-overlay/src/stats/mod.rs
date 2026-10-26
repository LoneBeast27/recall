//! Live numbers for the stats island: FPS + 1% low (Windows frame timing:
//! ETW present events, see `etw_session`), GPU usage + temperature (NVML),
//! CPU usage (GetSystemTimes). A poller exists only while the stats island is
//! on; dropping it stops the ETW session and unloads NVML.

pub mod cpu;
mod etw_session;
mod frame_math;
pub mod frame_timing;
pub mod nvml;
mod present_events;
pub mod probe;

use serde::Serialize;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use cpu::CpuMeter;
use frame_timing::{FpsSource, FrameTiming};
use nvml::Nvml;

pub use etw_session::install_cleanup;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StatsSnapshot {
    pub fps: Option<u32>,
    pub low1: Option<u32>,
    /// Where the FPS comes from right now (`no_permission` = FPS shows "—").
    pub fps_source: FpsSource,
    /// The process measured (the game, else the foreground window's).
    pub fps_pid: Option<u32>,
    pub gpu: Option<u32>,
    pub gpu_temp: Option<u32>,
    pub cpu: Option<u32>,
}

impl StatsSnapshot {
    /// A fixed snapshot from a smoke fixture's `"stats"` object.
    pub fn from_fixture(value: &serde_json::Value) -> Self {
        let num = |name: &str| value.get(name).and_then(serde_json::Value::as_u64).map(|n| n as u32);
        let fps_source = match value.get("fps_source").and_then(serde_json::Value::as_str) {
            Some("no_permission") => FpsSource::NoPermission,
            _ => FpsSource::Etw,
        };
        Self {
            fps: num("fps"),
            low1: num("low1"),
            fps_source,
            fps_pid: None,
            gpu: num("gpu"),
            gpu_temp: num("gpu_temp"),
            cpu: num("cpu"),
        }
    }
}

/// Lives exactly while the stats island is on: dropping it stops the frame
/// timing session and unloads NVML.
pub struct StatsPoller {
    nvml: Option<Nvml>,
    cpu: CpuMeter,
    frames: FrameTiming,
}

impl StatsPoller {
    pub fn start() -> Self {
        let mut cpu = CpuMeter::default();
        let _ = cpu.sample(); // prime the delta
        Self { nvml: Nvml::open(), cpu, frames: FrameTiming::new() }
    }

    pub fn nvml_loaded(&self) -> bool {
        self.nvml.is_some()
    }

    /// One 4 Hz sample. `pid` = the session target's process; without one
    /// (desktop recording) the foreground window's process is measured.
    pub fn poll(&mut self, pid: Option<u32>) -> StatsSnapshot {
        let fps_pid = pid.or_else(foreground_pid);
        let (fps_source, frames) = self.frames.poll(fps_pid);
        let gpu = self.nvml.as_ref().and_then(Nvml::sample);
        StatsSnapshot {
            fps: frames.fps,
            low1: frames.low1.map(|low| low.round() as u32),
            fps_source,
            fps_pid,
            gpu: gpu.map(|g| g.usage),
            gpu_temp: gpu.and_then(|g| g.temp_c),
            cpu: self.cpu.sample(),
        }
    }
}

/// The process owning the foreground window (never this overlay's own).
fn foreground_pid() -> Option<u32> {
    let mut pid = 0u32;
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    (pid != 0 && pid != std::process::id()).then_some(pid)
}
