//! Live numbers for the stats island: FPS + 1% low (RTSS shared memory), GPU
//! usage + temperature (NVML), CPU usage (GetSystemTimes). A poller exists
//! only while the stats island is on; dropping it unloads NVML.

pub mod cpu;
pub mod nvml;
pub mod rtss;
mod rtss_live;

use serde::Serialize;

use cpu::CpuMeter;
use nvml::Nvml;
use rtss::{Access, LowSource, RtssRead};
use rtss_live::RtssClient;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StatsSnapshot {
    pub fps: Option<u32>,
    pub low1: Option<u32>,
    pub low_source: Option<LowSource>,
    /// What the RTSS read found: no_memory | not_ready | no_entry | frames.
    pub rtss_read: String,
    /// How RTSS's mapping is reached (write = statistics switched on for the game).
    pub rtss_access: Option<Access>,
    pub gpu: Option<u32>,
    pub gpu_temp: Option<u32>,
    pub cpu: Option<u32>,
}

impl StatsSnapshot {
    /// A fixed snapshot from a smoke fixture's `"stats"` object.
    pub fn from_fixture(value: &serde_json::Value) -> Self {
        let num = |name: &str| value.get(name).and_then(serde_json::Value::as_u64).map(|n| n as u32);
        Self {
            fps: num("fps"),
            low1: num("low1"),
            low_source: None,
            rtss_read: "fixture".into(),
            rtss_access: None,
            gpu: num("gpu"),
            gpu_temp: num("gpu_temp"),
            cpu: num("cpu"),
        }
    }
}

/// Lives exactly while the stats island is on: dropping it unloads NVML and
/// switches RTSS's statistics back off for the game it switched them on for.
pub struct StatsPoller {
    nvml: Option<Nvml>,
    cpu: CpuMeter,
    rtss: RtssClient,
}

impl StatsPoller {
    pub fn start() -> Self {
        let mut cpu = CpuMeter::default();
        let _ = cpu.sample(); // prime the delta
        Self { nvml: Nvml::open(), cpu, rtss: RtssClient::default() }
    }

    pub fn nvml_loaded(&self) -> bool {
        self.nvml.is_some()
    }

    /// One 4 Hz sample. `pid` = the session target's process: RTSS records
    /// statistics for it (only for a real game pid, never the desktop's
    /// foreground-app fallback).
    pub fn poll(&mut self, pid: Option<u32>) -> StatsSnapshot {
        let rtss = self.rtss.poll(pid, pid);
        let gpu = self.nvml.as_ref().and_then(Nvml::sample);
        let (fps, low1, low_source) = match rtss {
            RtssRead::Frames(frames) => (
                frames.fps.map(|f| f.round() as u32),
                frames.low1.map(|f| f.round() as u32),
                Some(frames.low_source),
            ),
            _ => (None, None, None),
        };
        StatsSnapshot {
            fps,
            low1,
            low_source,
            rtss_read: rtss_read_name(&rtss).into(),
            rtss_access: self.rtss.access(),
            gpu: gpu.map(|g| g.usage),
            gpu_temp: gpu.and_then(|g| g.temp_c),
            cpu: self.cpu.sample(),
        }
    }
}

fn rtss_read_name(read: &RtssRead) -> &'static str {
    match read {
        RtssRead::NoMemory => "no_memory",
        RtssRead::NotReady => "not_ready",
        RtssRead::NoEntry => "no_entry",
        RtssRead::Frames(_) => "frames",
    }
}
