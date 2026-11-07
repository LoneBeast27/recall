//! The replay drain: after the game goes away, its engine is kept (not capturing:
//! the window is gone) while the replay ring still holds game footage, so Save
//! replay still saves the last clip. The session status says `"state":"draining"`
//! with a fixed end time and the seconds left as of that line (`remaining_secs`,
//! monotonic: a wall-clock jump does not move it). The app counts down locally,
//! from its arrival; the status is not re-broadcast every second (the dedup key,
//! `record_status::dedup_key`, leaves `remaining_secs` out).
//!
//! ```json
//! "drain": {"name": S, "total_secs": N, "ends_unix_ms": N, "remaining_secs": F} | null
//! ```

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::record_engine::EXIT_LINGER;
use crate::record_target::Target;

/// How long a closed game's engine is kept: its buffered footage, at most the
/// configured buffer; [`EXIT_LINGER`] while the engine has reported none.
pub fn drain_len(buffered_secs: Option<f32>, buffer_cap_secs: u32) -> Duration {
    match buffered_secs {
        Some(b) if b.is_finite() => Duration::from_secs_f32(b.clamp(0.0, buffer_cap_secs as f32)),
        _ => EXIT_LINGER,
    }
}

/// Turns the planner's drain into the status object. The wall-clock end is
/// computed once per drain, so every status during it carries the same value.
#[derive(Debug, Default)]
pub struct DrainClock {
    key: Option<Instant>,
    ends_unix_ms: u64,
}

impl DrainClock {
    pub fn status(&mut self, drain: Option<(&Target, Instant, Duration)>, now: Instant) -> Option<Value> {
        self.status_at(drain, now, unix_ms())
    }

    /// `status` with the wall clock given (`unix_now_ms`).
    pub fn status_at(&mut self, drain: Option<(&Target, Instant, Duration)>, now: Instant, unix_now_ms: u64) -> Option<Value> {
        let Some((target, started, len)) = drain else {
            self.key = None;
            return None;
        };
        let left = (started + len).saturating_duration_since(now);
        if self.key != Some(started) {
            self.key = Some(started);
            self.ends_unix_ms = unix_now_ms.saturating_add(left.as_millis() as u64);
        }
        Some(json!({
            "name": target.name,
            "total_secs": len.as_secs_f32().round() as u64,
            "ends_unix_ms": self.ends_unix_ms,
            "remaining_secs": left.as_millis() as f64 / 1000.0,
        }))
    }
}

fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// "2:00" / "0:45".
pub fn clock(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> Target {
        Target { hwnd: 7, pid: 42, name: "sobosuba".into(), exe: "sobosuba.exe".into() }
    }

    #[test]
    fn the_drain_lasts_the_buffered_footage_capped_at_the_buffer() {
        assert_eq!(drain_len(Some(120.0), 120), Duration::from_secs(120));
        assert_eq!(drain_len(Some(40.5), 120), Duration::from_secs_f32(40.5), "a short session drains what it has");
        assert_eq!(drain_len(Some(130.0), 120), Duration::from_secs(120));
        assert_eq!(drain_len(Some(0.0), 120), Duration::ZERO, "an encoder that never came up keeps nothing");
        assert_eq!(drain_len(None, 120), EXIT_LINGER);
        assert_eq!(drain_len(Some(f32::NAN), 120), EXIT_LINGER);
    }

    #[test]
    fn clock_text() {
        assert_eq!(clock(120), "2:00");
        assert_eq!(clock(45), "0:45");
    }

    #[test]
    fn the_status_end_time_is_fixed_for_one_drain() {
        let mut clock = DrainClock::default();
        let t0 = Instant::now();
        let g = game();
        assert_eq!(clock.status(None, t0), None);
        let a = clock.status(Some((&g, t0, Duration::from_secs(120))), t0).unwrap();
        let b = clock.status(Some((&g, t0, Duration::from_secs(120))), t0 + Duration::from_secs(3)).unwrap();
        assert_eq!(a["ends_unix_ms"], b["ends_unix_ms"], "the same end every tick");
        assert_eq!((a["remaining_secs"].as_f64(), b["remaining_secs"].as_f64()), (Some(120.0), Some(117.0)));
        assert_eq!(a["name"], "sobosuba");
        assert_eq!(a["total_secs"], 120);
        let ends = a["ends_unix_ms"].as_u64().unwrap();
        let now = unix_ms();
        assert!(ends > now + 115_000 && ends <= now + 120_000, "ends {ends} vs now {now}");
        assert_eq!(clock.status(None, t0), None);
        let c = clock.status(Some((&g, t0 + Duration::from_secs(9), Duration::from_secs(30))), t0 + Duration::from_secs(9)).unwrap();
        assert_eq!(c["total_secs"], 30, "a new drain gets a new end");
    }

    #[test]
    fn the_remaining_time_follows_the_monotonic_clock_not_the_wall_clock() {
        let mut clock = DrainClock::default();
        let t0 = Instant::now();
        let g = game();
        let unix0 = 1_790_000_000_000u64;
        let a = clock.status_at(Some((&g, t0, Duration::from_secs(120))), t0, unix0).unwrap();
        assert_eq!(a["remaining_secs"], 120.0);
        // 10 s later the wall clock has jumped an hour ahead.
        let b = clock.status_at(Some((&g, t0, Duration::from_secs(120))), t0 + Duration::from_secs(10), unix0 + 3_610_000).unwrap();
        assert_eq!(b["remaining_secs"], 110.0, "110 s left, whatever the wall clock says");
        assert_eq!(b["ends_unix_ms"], unix0 + 120_000, "the old field stays for older apps");
        let done = clock.status_at(Some((&g, t0, Duration::from_secs(120))), t0 + Duration::from_secs(130), unix0).unwrap();
        assert_eq!(done["remaining_secs"], 0.0);
    }
}
