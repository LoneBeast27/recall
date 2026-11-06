//! Test-only faults for the live encoder loop (`EncStats::faults`): failed opens, failed
//! encodes and a removed D3D11 device, so recovery is exercised on the real loop.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::Duration;

/// What DXGI reports through FFmpeg once the device is gone (TDR, driver update).
pub(crate) const DEVICE_REMOVED: &str = "Generic error in an external library (DXGI_ERROR_DEVICE_REMOVED 0x887A0005)";

#[derive(Default)]
pub(crate) struct Faults {
    /// The next this-many opens fail.
    pub open_errors: AtomicU32,
    /// The next this-many encodes fail.
    pub encode_errors: AtomicU32,
    /// Opens on frames from this pool fail: its device was removed.
    pub dead_pool: Mutex<Option<usize>>,
    /// Retry delays 1/20 of the real ones.
    pub fast: AtomicBool,
}

impl Faults {
    pub fn open(&self, pool: Option<usize>) -> Option<String> {
        if pool.is_some() && *self.dead_pool.lock().unwrap() == pool {
            return Some(DEVICE_REMOVED.into());
        }
        take(&self.open_errors).then(|| "injected open failure".into())
    }

    pub fn encode(&self) -> Option<String> {
        take(&self.encode_errors).then(|| DEVICE_REMOVED.into())
    }

    /// GPU-path retries 1/60 of the real ones (1 min -> 1 s).
    pub fn probe_delays(&self, real: [Duration; 4]) -> [Duration; 4] {
        if self.fast.load(Relaxed) {
            real.map(|d| d / 60)
        } else {
            real
        }
    }

    pub fn delays(&self, real: [Duration; 4]) -> [Duration; 4] {
        if self.fast.load(Relaxed) {
            real.map(|d| d / 20)
        } else {
            real
        }
    }
}

fn take(n: &AtomicU32) -> bool {
    n.fetch_update(Relaxed, Relaxed, |v| v.checked_sub(1)).is_ok()
}
