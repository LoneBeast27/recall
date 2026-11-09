//! `--frame-timing-probe PID [--probe-seconds N]`: runs the frame timing
//! session for one process, windowless (no island, no `--parent`), and prints
//! a JSON line per 250 ms poll (FPS, 1% low, source, the trace note when the
//! session's state changed), then a last line with this process's CPU time.
//! The instrument for the permission path, the numbers outside a game, and
//! the filtered vs unfiltered CPU A/B (`RECALL_ETW_NO_PID_FILTER=1` on the
//! second run); stops early when the session is refused.

use std::time::{Duration, Instant};

use serde_json::json;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

use super::frame_timing::{FpsSource, FrameTiming};

pub fn run(pid: u32, seconds: u64) -> Result<(), String> {
    let mut timing = FrameTiming::new();
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
    println!("{}", json!({ "done": true, "wall_ms": wall_ms, "cpu_ms": cpu_ms() }));
    Ok(())
}

/// This process's user + kernel CPU time so far, in ms.
fn cpu_ms() -> Option<f64> {
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Some((ticks(kernel) + ticks(user)) as f64 / 10_000.0)
}
