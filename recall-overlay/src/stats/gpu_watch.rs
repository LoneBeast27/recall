//! The GPU reading behind the stats pill, kept alive: after [`FAIL_LIMIT`]
//! failed samples in a row (a TDR, a driver update: NVML reports the GPU
//! lost until it is opened again) the handle is dropped and opened again at
//! most every [`REOPEN_EVERY`]. A handle that opens but never samples (NVML
//! present, utilization not supported) backs off: 10 s, 1 min, then 5 min.

use std::time::{Duration, Instant};

use super::nvml::GpuSample;

/// Failed samples in a row before the handle is dropped (about 2 s at 4 Hz).
pub const FAIL_LIMIT: u32 = 8;
/// The least time between two opens.
pub const REOPEN_EVERY: Duration = Duration::from_secs(10);
/// The wait before reopening after 1, 2, 3+ handles in a row never sampled.
const BACKOFF: [Duration; 3] = [REOPEN_EVERY, Duration::from_secs(60), Duration::from_secs(300)];

/// The least time between opens after `strikes` handles in a row never sampled.
fn wait(strikes: usize) -> Duration {
    BACKOFF[strikes.saturating_sub(1).min(BACKOFF.len() - 1)]
}

/// Something that reads the GPU (NVML; a fake in tests).
pub trait GpuSource {
    fn sample(&self) -> Option<GpuSample>;
}

/// What a dropped watch remembers about its opens, so the next stats pill's
/// watch keeps the back-off instead of opening NVML again at once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuBackoff {
    opened_at: Instant,
    strikes: usize,
    /// The last handle was open and had sampled: open again at once.
    healthy: bool,
}

pub struct GpuWatch<G> {
    gpu: Option<G>,
    open: Box<dyn Fn() -> Option<G>>,
    failures: u32,
    opened_at: Instant,
    /// The open handle gave at least one sample.
    sampled: bool,
    /// Handles in a row dropped without ever sampling.
    strikes: usize,
}

impl<G: GpuSource> GpuWatch<G> {
    pub fn start(open: Box<dyn Fn() -> Option<G>>, now: Instant) -> Self {
        Self { gpu: open(), open, failures: 0, opened_at: now, sampled: false, strikes: 0 }
    }

    /// A watch for a new stats pill: opens now unless the last one's back-off
    /// (`was`) still runs, in which case it opens when that wait ends.
    pub fn resume(open: Box<dyn Fn() -> Option<G>>, was: Option<GpuBackoff>, now: Instant) -> Self {
        match was {
            Some(b) if !b.healthy && now.saturating_duration_since(b.opened_at) < wait(b.strikes) => {
                Self { gpu: None, open, failures: 0, opened_at: b.opened_at, sampled: false, strikes: b.strikes }
            }
            Some(b) => Self { strikes: b.strikes, ..Self::start(open, now) },
            None => Self::start(open, now),
        }
    }

    /// What the next watch needs when this one is dropped. An open handle that
    /// failed and never sampled counts as a strike, as if it ran to `FAIL_LIMIT`.
    pub fn backoff(&self) -> GpuBackoff {
        let open = self.gpu.is_some();
        let never = open && !self.sampled && self.failures > 0;
        GpuBackoff { opened_at: self.opened_at, strikes: self.strikes + never as usize, healthy: open && self.sampled }
    }

    pub fn loaded(&self) -> bool {
        self.gpu.is_some()
    }

    /// One sample; a handle that keeps failing is dropped, and a missing one
    /// is opened again once [`REOPEN_EVERY`] has passed since the last open.
    pub fn sample(&mut self, now: Instant) -> Option<GpuSample> {
        if self.gpu.is_none() && now.saturating_duration_since(self.opened_at) >= wait(self.strikes) {
            self.opened_at = now;
            self.gpu = (self.open)();
            (self.failures, self.sampled) = (0, false);
        }
        let sample = self.gpu.as_ref().and_then(GpuSource::sample);
        if self.gpu.is_some() {
            self.failures = if sample.is_some() { 0 } else { self.failures + 1 };
            self.sampled |= sample.is_some();
            if self.failures >= FAIL_LIMIT {
                self.strikes = if self.sampled { 0 } else { self.strikes + 1 };
                self.gpu = None; // dropped: NVML shuts down before the next open
            }
        }
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    struct Fake(Rc<Cell<bool>>);

    impl GpuSource for Fake {
        fn sample(&self) -> Option<GpuSample> {
            self.0.get().then_some(GpuSample { usage: 50, temp_c: Some(60) })
        }
    }

    #[test]
    fn a_lost_gpu_is_opened_again_after_eight_failures() {
        let now = Instant::now();
        let (ok, opens) = (Rc::new(Cell::new(true)), Rc::new(Cell::new(0)));
        let (o, n) = (ok.clone(), opens.clone());
        let mut watch = GpuWatch::start(Box::new(move || { n.set(n.get() + 1); Some(Fake(o.clone())) }), now);
        assert!(watch.sample(now).is_some());
        ok.set(false); // the GPU is lost
        let tick = |i: u32| now + Duration::from_millis(250 * u64::from(i));
        for i in 1..=FAIL_LIMIT {
            assert!(watch.sample(tick(i)).is_none());
        }
        assert_eq!(opens.get(), 1, "no re-open storm");
        ok.set(true); // the driver is back
        let later = now + REOPEN_EVERY + Duration::from_secs(3);
        assert!(watch.sample(later).is_some(), "the ring comes back without toggling stats");
        assert_eq!(opens.get(), 2);
    }

    #[test]
    fn a_missing_driver_is_retried_only_every_ten_seconds() {
        let now = Instant::now();
        let opens = Rc::new(Cell::new(0));
        let n = opens.clone();
        let mut watch: GpuWatch<Fake> = GpuWatch::start(Box::new(move || { n.set(n.get() + 1); None }), now);
        for i in 0..40 {
            assert!(watch.sample(now + Duration::from_millis(250 * i)).is_none());
        }
        assert_eq!(opens.get(), 1, "within the first 10 s");
        watch.sample(now + REOPEN_EVERY);
        assert_eq!(opens.get(), 2);
    }

    #[test]
    fn a_gpu_that_opens_but_never_samples_backs_off() {
        let now = Instant::now();
        let opens = Rc::new(Cell::new(0));
        let n = opens.clone();
        let never = Rc::new(Cell::new(false));
        let mut watch = GpuWatch::start(Box::new(move || { n.set(n.get() + 1); Some(Fake(never.clone())) }), now);
        for i in 0..480 {
            assert!(watch.sample(now + Duration::from_millis(250 * i)).is_none());
        }
        assert!(opens.get() <= 3, "{} opens in two minutes", opens.get());
    }

    #[test]
    fn a_gpu_that_sampled_before_is_reopened_after_ten_seconds_again() {
        let now = Instant::now();
        let (ok, opens) = (Rc::new(Cell::new(true)), Rc::new(Cell::new(0)));
        let (o, n) = (ok.clone(), opens.clone());
        let mut watch = GpuWatch::start(Box::new(move || { n.set(n.get() + 1); Some(Fake(o.clone())) }), now);
        let tick = |i: u64| now + Duration::from_millis(250 * i);
        for round in 0..3u64 {
            let base = round * 60;
            assert!(watch.sample(tick(base)).is_some() || round > 0);
            ok.set(false);
            for i in 1..=u64::from(FAIL_LIMIT) {
                watch.sample(tick(base + i));
            }
            ok.set(true);
        }
        assert_eq!(opens.get(), 3, "each loss after real samples reopens at the 10 s step");
    }

    #[test]
    fn a_new_pill_keeps_the_back_off_of_the_last_one() {
        let now = Instant::now();
        let s = Duration::from_secs;
        let opens = Rc::new(Cell::new(0));
        let n = opens.clone();
        let never = Rc::new(Cell::new(false));
        let open = move || -> Box<dyn Fn() -> Option<Fake>> {
            let (n, never) = (n.clone(), never.clone());
            Box::new(move || { n.set(n.get() + 1); Some(Fake(never.clone())) })
        };
        // Shows the pill from `at` for 2 s (8 samples), then drops the poller.
        let show = |was: Option<GpuBackoff>, at: Instant| {
            let mut watch = GpuWatch::resume(open(), was, at);
            for i in 0..8 {
                watch.sample(at + Duration::from_millis(250 * i));
            }
            watch.backoff()
        };
        let was = show(None, now);
        assert_eq!(opens.get(), 1);
        let was = show(Some(was), now + s(5));
        assert_eq!(opens.get(), 1, "an alt-tab 5 s later does not reopen NVML (10 s step)");
        let was = show(Some(was), now + s(12));
        assert_eq!(opens.get(), 2, "the 10 s step, as one unbroken pill would");
        let was = show(Some(was), now + s(20));
        assert_eq!(opens.get(), 2, "then 60 s");
        show(Some(was), now + s(73));
        assert_eq!(opens.get(), 3);
    }

    #[test]
    fn a_working_gpu_opens_at_once_on_the_next_pill() {
        let now = Instant::now();
        let opens = Rc::new(Cell::new(0));
        let n = opens.clone();
        let make = move || -> Box<dyn Fn() -> Option<Fake>> {
            let n = n.clone();
            Box::new(move || { n.set(n.get() + 1); Some(Fake(Rc::new(Cell::new(true)))) })
        };
        let mut watch = GpuWatch::resume(make(), None, now);
        assert!(watch.sample(now).is_some());
        let was = watch.backoff();
        drop(watch);
        let mut again = GpuWatch::resume(make(), Some(was), now + Duration::from_secs(3));
        assert!(again.sample(now + Duration::from_secs(3)).is_some(), "the GPU reading is back with the pill");
        assert_eq!(opens.get(), 2);
    }

    #[test]
    fn a_missing_driver_is_not_retried_on_every_pill() {
        let now = Instant::now();
        let opens = Rc::new(Cell::new(0));
        let n = opens.clone();
        let make = move || -> Box<dyn Fn() -> Option<Fake>> {
            let n = n.clone();
            Box::new(move || { n.set(n.get() + 1); None })
        };
        let was = GpuWatch::resume(make(), None, now).backoff();
        let mut again = GpuWatch::resume(make(), Some(was), now + Duration::from_secs(4));
        again.sample(now + Duration::from_secs(4));
        assert_eq!(opens.get(), 1, "at most every 10 s");
        again.sample(now + REOPEN_EVERY);
        assert_eq!(opens.get(), 2);
    }
}
