//! The GPU reading behind the stats pill, kept alive: after [`FAIL_LIMIT`]
//! failed samples in a row (a TDR, a driver update: NVML reports the GPU
//! lost until it is opened again) the handle is dropped and opened again at
//! most every [`REOPEN_EVERY`].

use std::time::{Duration, Instant};

use super::nvml::GpuSample;

/// Failed samples in a row before the handle is dropped (about 2 s at 4 Hz).
pub const FAIL_LIMIT: u32 = 8;
/// The least time between two opens.
pub const REOPEN_EVERY: Duration = Duration::from_secs(10);

/// Something that reads the GPU (NVML; a fake in tests).
pub trait GpuSource {
    fn sample(&self) -> Option<GpuSample>;
}

pub struct GpuWatch<G> {
    gpu: Option<G>,
    open: Box<dyn Fn() -> Option<G>>,
    failures: u32,
    opened_at: Instant,
}

impl<G: GpuSource> GpuWatch<G> {
    pub fn start(open: Box<dyn Fn() -> Option<G>>, now: Instant) -> Self {
        Self { gpu: open(), open, failures: 0, opened_at: now }
    }

    pub fn loaded(&self) -> bool {
        self.gpu.is_some()
    }

    /// One sample; a handle that keeps failing is dropped, and a missing one
    /// is opened again once [`REOPEN_EVERY`] has passed since the last open.
    pub fn sample(&mut self, now: Instant) -> Option<GpuSample> {
        if self.gpu.is_none() && now.saturating_duration_since(self.opened_at) >= REOPEN_EVERY {
            self.opened_at = now;
            self.gpu = (self.open)();
            self.failures = 0;
        }
        let sample = self.gpu.as_ref().and_then(GpuSource::sample);
        if self.gpu.is_some() {
            self.failures = if sample.is_some() { 0 } else { self.failures + 1 };
            if self.failures >= FAIL_LIMIT {
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
}
