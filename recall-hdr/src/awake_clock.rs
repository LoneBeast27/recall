//! One clock that stops while the PC sleeps (design-detect-release §0): every duration in the
//! privacy release's settle, stall and kill path runs on it, in both the engine (`recall`) and
//! the controller (`recall-hdr`), which share this file through `#[path]`.
//!
//! `std::time::Instant` is `QueryPerformanceCounter` on Windows, and nothing promises that it
//! leaves sleep out (Microsoft's own wording says QPC counts it). A save that was healthy when
//! the lid closed must not look stalled for the length of the sleep when it opens, so the
//! timers here read `QueryUnbiasedInterruptTime`: "does not include time the system spends in
//! sleep or hibernation" (Windows 7 on; 100 ns units, one clock tick of resolution).
//!
//! Code takes a [`Clock`]; production passes [`AwakeClock`], tests a `FakeClock` whose awake
//! and asleep time move separately (`sleep_for` moves only the biased clock).

use std::ops::{Add, AddAssign};
use std::time::{Duration, Instant};

/// A point on the awake-only clock, in 100 ns units since boot (sleep left out).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Awake(u64);

impl Awake {
    /// The clock's origin (tests count from here).
    pub const ZERO: Awake = Awake(0);

    /// `ticks` in 100 ns units.
    pub const fn from_ticks(ticks: u64) -> Awake {
        Awake(ticks)
    }

    /// The awake time from `earlier` to `self`; zero when `earlier` is later.
    pub fn saturating_duration_since(self, earlier: Awake) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0).saturating_mul(100))
    }

    /// Same as [`Self::saturating_duration_since`] (never panics, unlike `Instant`'s).
    pub fn duration_since(self, earlier: Awake) -> Duration {
        self.saturating_duration_since(earlier)
    }
}

/// `d` in the clock's 100 ns ticks (saturating).
fn ticks(d: Duration) -> u64 {
    u64::try_from(d.as_nanos() / 100).unwrap_or(u64::MAX)
}

impl Add<Duration> for Awake {
    type Output = Awake;
    fn add(self, d: Duration) -> Awake {
        Awake(self.0.saturating_add(ticks(d)))
    }
}

impl AddAssign<Duration> for Awake {
    fn add_assign(&mut self, d: Duration) {
        *self = *self + d;
    }
}

/// Where code reads time: the awake-only clock, and the ordinary one for anything that is
/// not a duration of the release path (a log line's "slept for").
pub trait Clock: Send + Sync {
    /// Now on the awake-only clock.
    fn awake(&self) -> Awake;
    /// Now on the ordinary (sleep-counting) clock.
    fn instant(&self) -> Instant;
}

/// The real clocks.
#[derive(Debug, Clone, Copy, Default)]
pub struct AwakeClock;

#[link(name = "kernel32")]
extern "system" {
    fn QueryUnbiasedInterruptTime(unbiased_time: *mut u64) -> i32;
}

impl Clock for AwakeClock {
    fn awake(&self) -> Awake {
        let mut ticks = 0u64;
        // SAFETY: writes one u64 through a valid pointer; cannot fail on Windows 7 or later.
        unsafe { QueryUnbiasedInterruptTime(&mut ticks) };
        Awake(ticks)
    }
    fn instant(&self) -> Instant {
        Instant::now()
    }
}

/// Now on the real awake-only clock.
pub fn awake_now() -> Awake {
    AwakeClock.awake()
}

/// Tests: awake and asleep time move separately, by hand or on every read.
#[cfg(test)]
#[derive(Debug)]
pub struct FakeClock {
    base: Instant,
    /// Awake / asleep time so far, in nanoseconds.
    awake_ns: std::sync::atomic::AtomicU64,
    asleep_ns: std::sync::atomic::AtomicU64,
    /// Moved on by each [`Clock::awake`] read (a scripted run of polls).
    step_awake_ns: u64,
    step_asleep_ns: u64,
    /// [`Clock::awake`] reads so far.
    reads: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
impl FakeClock {
    pub fn new() -> FakeClock {
        FakeClock::stepping(Duration::ZERO, Duration::ZERO)
    }

    /// Each awake read moves the clock on by `awake` awake and `asleep` asleep first.
    pub fn stepping(awake: Duration, asleep: Duration) -> FakeClock {
        use std::sync::atomic::AtomicU64;
        FakeClock {
            base: Instant::now(),
            awake_ns: AtomicU64::new(0),
            asleep_ns: AtomicU64::new(0),
            step_awake_ns: awake.as_nanos() as u64,
            step_asleep_ns: asleep.as_nanos() as u64,
            reads: AtomicU64::new(0),
        }
    }

    /// The PC runs for `d`: both clocks move.
    pub fn advance_awake(&self, d: Duration) {
        self.awake_ns.fetch_add(d.as_nanos() as u64, std::sync::atomic::Ordering::SeqCst);
    }

    /// The PC sleeps for `d`: only the ordinary clock moves.
    pub fn sleep_for(&self, d: Duration) {
        self.asleep_ns.fetch_add(d.as_nanos() as u64, std::sync::atomic::Ordering::SeqCst);
    }

    /// How many times the awake clock was read.
    pub fn reads(&self) -> u64 {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Wait (real time, bounded) until the clock was read at least `n` times.
    pub fn wait_reads(&self, n: u64) {
        let until = Instant::now() + Duration::from_secs(10);
        while self.reads() < n && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(self.reads() >= n, "the clock was read {} times, waited for {n}", self.reads());
    }
}

#[cfg(test)]
impl Clock for FakeClock {
    fn awake(&self) -> Awake {
        use std::sync::atomic::Ordering::SeqCst;
        self.awake_ns.fetch_add(self.step_awake_ns, SeqCst);
        self.asleep_ns.fetch_add(self.step_asleep_ns, SeqCst);
        self.reads.fetch_add(1, SeqCst);
        Awake::ZERO + Duration::from_nanos(self.awake_ns.load(SeqCst))
    }
    fn instant(&self) -> Instant {
        use std::sync::atomic::Ordering::SeqCst;
        self.base + Duration::from_nanos(self.awake_ns.load(SeqCst) + self.asleep_ns.load(SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awake_time_adds_and_subtracts_in_100_ns_units_and_never_panics() {
        let t = Awake::ZERO + Duration::from_secs(3);
        assert_eq!(t, Awake::from_ticks(30_000_000));
        assert_eq!(t.saturating_duration_since(Awake::ZERO), Duration::from_secs(3));
        assert_eq!(Awake::ZERO.saturating_duration_since(t), Duration::ZERO, "earlier minus later is zero");
        assert_eq!(Awake::from_ticks(u64::MAX - 1) + Duration::from_secs(1), Awake::from_ticks(u64::MAX), "saturates");
    }

    #[test]
    fn the_real_awake_clock_moves_forward() {
        let a = awake_now();
        std::thread::sleep(Duration::from_millis(40));
        let b = awake_now();
        assert!(b > a && b.saturating_duration_since(a) < Duration::from_secs(5), "{a:?} -> {b:?}");
    }

    #[test]
    fn a_fake_sleep_moves_only_the_ordinary_clock() {
        let clock = FakeClock::new();
        let (a0, i0) = (clock.awake(), clock.instant());
        clock.sleep_for(Duration::from_secs(3600));
        assert_eq!(clock.awake(), a0, "asleep: the awake clock stands still");
        assert_eq!(clock.instant() - i0, Duration::from_secs(3600));
        clock.advance_awake(Duration::from_secs(2));
        assert_eq!(clock.awake().saturating_duration_since(a0), Duration::from_secs(2));
        assert_eq!(clock.instant() - i0, Duration::from_secs(3602), "awake: both move");
    }

    #[test]
    fn a_stepping_fake_moves_on_every_read() {
        let clock = FakeClock::stepping(Duration::from_secs(1), Duration::from_secs(3600));
        let a = clock.awake();
        let b = clock.awake();
        assert_eq!(b.saturating_duration_since(a), Duration::from_secs(1));
        assert_eq!(clock.reads(), 2);
        assert!(clock.instant() - clock.base >= Duration::from_secs(7200));
    }
}
