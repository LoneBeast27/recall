//! System CPU usage from `GetSystemTimes` deltas (kernel time includes idle).

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::GetSystemTimes;

/// (idle, kernel + user) in 100 ns units since boot.
pub type Times = (u64, u64);

#[derive(Default)]
pub struct CpuMeter {
    last: Option<Times>,
    smoothed: Option<f32>,
}

impl CpuMeter {
    /// Usage over the time since the previous call, lightly smoothed; `None` on the first call.
    pub fn sample(&mut self) -> Option<u32> {
        let now = system_times()?;
        let usage = self.last.and_then(|prev| usage_between(prev, now));
        self.last = Some(now);
        let usage = usage? as f32;
        let next = match self.smoothed {
            Some(prev) => prev * 0.4 + usage * 0.6,
            None => usage,
        };
        self.smoothed = Some(next);
        Some(next.round().clamp(0.0, 100.0) as u32)
    }
}

/// Percent busy between two `GetSystemTimes` readings.
pub fn usage_between(prev: Times, now: Times) -> Option<u32> {
    let idle = now.0.checked_sub(prev.0)?;
    let total = now.1.checked_sub(prev.1)?;
    if total == 0 {
        return None;
    }
    let busy = total.saturating_sub(idle) as f64;
    Some((busy * 100.0 / total as f64).round().clamp(0.0, 100.0) as u32)
}

fn system_times() -> Option<Times> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut idle as *mut _), Some(&mut kernel as *mut _), Some(&mut user as *mut _)) }.ok()?;
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Some((ticks(idle), ticks(kernel) + ticks(user)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_busy_share_of_total_time() {
        assert_eq!(usage_between((0, 0), (750, 1_000)), Some(25));
        assert_eq!(usage_between((100, 1_000), (100, 2_000)), Some(100));
        assert_eq!(usage_between((5, 10), (5, 10)), None, "no time passed");
        assert_eq!(usage_between((5, 10), (4, 20)), None, "counters never go backwards");
    }

    #[test]
    fn live_meter_reports_after_the_first_sample() {
        let mut meter = CpuMeter::default();
        assert_eq!(meter.sample(), None);
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert!(meter.sample().is_some_and(|usage| usage <= 100));
    }
}
