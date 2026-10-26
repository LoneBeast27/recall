//! `--frame-timing-probe PID [--probe-seconds N]`: runs the frame timing
//! session for one process and prints a JSON line per 250 ms poll (FPS,
//! 1% low, source). The instrument for checking the permission path and the
//! numbers outside a game; stops early when the session is refused.

use std::time::{Duration, Instant};

use serde_json::json;

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
        });
        println!("{line}");
        if source == FpsSource::NoPermission {
            return Err("frame timing refused: not an elevated administrator and not in Performance Log Users".into());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}
