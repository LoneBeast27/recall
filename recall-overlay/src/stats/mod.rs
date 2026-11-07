//! Live numbers for the stats island: FPS + 1% low (Windows frame timing:
//! ETW present events, see `etw_session`), GPU usage + temperature (NVML),
//! CPU usage (GetSystemTimes). A poller exists only while the stats island is
//! on; dropping it stops the ETW session and unloads NVML.

pub mod cpu;
mod etw_session;
mod frame_math;
pub mod frame_timing;
pub mod gpu_watch;
pub mod nvml;
mod present_events;
pub mod probe;

use std::time::Instant;

use serde::Serialize;

use cpu::CpuMeter;
use frame_timing::{FpsSource, FrameTiming};
use gpu_watch::GpuWatch;
use nvml::Nvml;

pub use etw_session::install_cleanup;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StatsSnapshot {
    pub fps: Option<u32>,
    pub low1: Option<u32>,
    /// Where the FPS comes from right now (`no_permission` = FPS shows "—").
    pub fps_source: FpsSource,
    /// The process measured (the game).
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
    nvml: GpuWatch<Nvml>,
    cpu: CpuMeter,
    frames: FrameTiming,
}

impl StatsPoller {
    pub fn start() -> Self {
        let mut cpu = CpuMeter::default();
        let _ = cpu.sample(); // prime the delta
        Self { nvml: GpuWatch::start(Box::new(Nvml::open), Instant::now()), cpu, frames: FrameTiming::new() }
    }

    pub fn nvml_loaded(&self) -> bool {
        self.nvml.loaded()
    }

    /// The frame timing session's last phase change (for the trace log).
    pub fn take_note(&mut self) -> Option<String> {
        self.frames.take_note()
    }

    /// One 4 Hz sample. `pid` = the game's process (the poller runs only
    /// while the stats pill can show, so never for the desktop).
    pub fn poll(&mut self, pid: Option<u32>) -> StatsSnapshot {
        let fps_pid = pid;
        let (fps_source, frames) = self.frames.poll(fps_pid);
        let gpu = self.nvml.sample(Instant::now());
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
