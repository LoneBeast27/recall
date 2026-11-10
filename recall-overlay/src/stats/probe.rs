//! `--frame-timing-probe PID [--probe-seconds N]`: runs the frame timing
//! session for one process, windowless (no island, no `--parent`), and prints
//! a JSON line per 250 ms poll (FPS, 1% low, source, the trace note when the
//! session's state changed), then a last line with this process's CPU time.
//! The instrument for the permission path, the numbers outside a game, and
//! the filtered vs unfiltered CPU A/B (`RECALL_ETW_NO_PID_FILTER=1` on the
//! second run); stops early when the session is refused.
//!
//! Its session is `RecallFrameTimingProbe`, never the overlay's, so probing
//! while the installed overlay runs leaves the island's numbers alone.
//!
//! The CPU line: `cpu_ms` = this process's CPU cycles during the run
//! (`QueryProcessCycleTime`) over the TSC rate measured against QPC across
//! the same run; `cpu_ms_coarse` = `GetProcessTimes` (15.6 ms steps: only
//! for long runs). recall-overlay is a GUI-subsystem exe, so a launcher that
//! does not wait would cut it short: through the build gate run it as
//! `gated.ps1 none cmd /c recall-overlay.exe --frame-timing-probe PID --probe-seconds 60`.

use std::time::{Duration, Instant};

use serde_json::json;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

// windows 0.62 puts this in System_WindowsProgramming, a feature the crate does not enable.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryProcessCycleTime(process: HANDLE, cycle_time: *mut u64) -> i32;
}

use super::frame_timing::{FpsSource, FrameTiming};

pub fn run(pid: u32, seconds: u64) -> Result<(), String> {
    let mut timing = FrameTiming::probe();
    let cpu_at_start = CpuMark::now();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        let (source, stats) = timing.poll(Some(pid));
        let line = json!({
            "t_ms": started.elapsed().as_millis() as u64,
            "pid": pid,
            "fps_source": source,
            "fps": stats.fps,
            "low1": stats.low1.map(|low| (low * 10.0).round() / 10.0),
            "note": timing.take_note(),
        });
        println!("{line}");
        if source == FpsSource::NoPermission {
            return Err("frame timing refused: not an elevated administrator and not in Performance Log Users".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    drop(timing); // stop the session before the CPU reading
    let wall_ms = started.elapsed().as_millis() as u64;
    let cpu = CpuMark::now().since(&cpu_at_start);
    let round = |ms: Option<f64>| ms.map(|ms| (ms * 1000.0).round() / 1000.0);
    let line = json!({
        "done": true,
        "wall_ms": wall_ms,
        "cpu_ms": round(cpu.cycles_ms),
        "cpu_ms_coarse": round(cpu.coarse_ms),
        "tsc_mhz": round(cpu.tsc_mhz),
    });
    println!("{line}");
    Ok(())
}

/// One reading of this process's CPU use and the clocks to scale it.
struct CpuMark {
    cycles: Option<u64>,
    /// `GetProcessTimes` user + kernel, 100 ns units.
    times: Option<u64>,
    tsc: Option<u64>,
    qpc: i64,
}

struct CpuUse {
    cycles_ms: Option<f64>,
    coarse_ms: Option<f64>,
    tsc_mhz: Option<f64>,
}

impl CpuMark {
    fn now() -> Self {
        let process = unsafe { GetCurrentProcess() };
        let mut cycles = 0u64;
        let cycles = (unsafe { QueryProcessCycleTime(process, &mut cycles) } != 0).then_some(cycles);
        let (mut created, mut exited, mut kernel, mut user) = Default::default();
        let times = unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }
            .ok()
            .map(|()| filetime(kernel) + filetime(user));
        let mut qpc = 0i64;
        let _ = unsafe { QueryPerformanceCounter(&mut qpc) };
        Self { cycles, times, tsc: tsc(), qpc }
    }

    fn since(&self, start: &CpuMark) -> CpuUse {
        let mut qpf = 0i64;
        let _ = unsafe { QueryPerformanceFrequency(&mut qpf) };
        let secs = (self.qpc - start.qpc) as f64 / qpf.max(1) as f64;
        let delta = |end: Option<u64>, begin: Option<u64>| Some(end?.checked_sub(begin?)? as f64);
        let tsc_hz = delta(self.tsc, start.tsc).filter(|_| secs > 0.0).map(|ticks| ticks / secs);
        let cycles_ms = delta(self.cycles, start.cycles).zip(tsc_hz).map(|(cycles, hz)| cycles / hz * 1000.0);
        let coarse_ms = delta(self.times, start.times).map(|units| units / 10_000.0);
        CpuUse { cycles_ms, coarse_ms, tsc_mhz: tsc_hz.map(|hz| hz / 1e6) }
    }
}

fn filetime(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

/// The time-stamp counter (Windows' cycle counts tick at its rate).
#[cfg(target_arch = "x86_64")]
fn tsc() -> Option<u64> {
    Some(unsafe { std::arch::x86_64::_rdtsc() })
}

#[cfg(not(target_arch = "x86_64"))]
fn tsc() -> Option<u64> {
    None
}
