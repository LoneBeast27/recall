//! Pure decisions for the Desktop Duplication capture (`dda`): frame pacing, format choice,
//! what to do on each DXGI error, and the clock conversion. No GPU calls, so all of it is
//! unit tested.

use std::time::{Duration, Instant};

use windows::core::HRESULT;
use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_MODE_ROTATION,
    DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_DENIED, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_HUNG, DXGI_ERROR_DEVICE_REMOVED,
    DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_DRIVER_INTERNAL_ERROR, DXGI_ERROR_WAIT_TIMEOUT,
};

/// Delivers frames at most at the capture rate, on a schedule rather than "interval since the
/// last frame": on a 144 Hz desktop with a 66 fps cap, spacing from the last frame would keep
/// only every third vsync (48 fps); the schedule keeps the average at the cap.
pub(super) struct Pacer {
    interval: Duration,
    next_due: Option<Instant>,
}

/// What to do with a new desktop frame.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Pace {
    Deliver,
    /// Too early: release it, and wait this long before acquiring again.
    Skip(Duration),
}

impl Pacer {
    pub(super) fn new(interval: Duration) -> Self {
        Self { interval, next_due: None }
    }

    pub(super) fn offer(&mut self, now: Instant) -> Pace {
        match self.next_due {
            Some(due) if now < due => Pace::Skip(due - now),
            due => {
                // Stay on the schedule unless we fell more than one interval behind (an idle
                // desktop), then restart it from now so a burst does not follow.
                let next = due.map(|d| d + self.interval).filter(|n| *n > now).unwrap_or(now + self.interval);
                self.next_due = Some(next);
                Pace::Deliver
            }
        }
    }
}

/// The one format asked of `DuplicateOutput1`: exactly what the capture start picked
/// (`CaptureShared::start_format`), so the pipeline and the CPU path see the same pixels as
/// with WGC. Offering both would let DXGI hand Bgra8 to a capture that needs fp16.
pub(super) fn duplication_format(fp16: bool) -> DXGI_FORMAT {
    if fp16 { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM }
}

/// The old `DuplicateOutput` (no format list) always gives Bgra8, so it is a fallback for
/// Bgra8 captures only.
pub(super) fn legacy_duplication_fits(fp16: bool) -> bool {
    !fp16
}

pub(super) fn format_label(format: DXGI_FORMAT) -> &'static str {
    match format {
        DXGI_FORMAT_R16G16B16A16_FLOAT => "Rgba16F",
        DXGI_FORMAT_B8G8R8A8_UNORM => "Bgra8",
        _ => "other",
    }
}

/// A rotated monitor's duplicated image comes in the panel's own orientation; the pipeline
/// does not rotate, so those monitors stay on WGC.
pub(super) fn rotation_supported(rotation: DXGI_MODE_ROTATION) -> bool {
    rotation == DXGI_MODE_ROTATION_IDENTITY || rotation == DXGI_MODE_ROTATION_UNSPECIFIED
}

/// What an `AcquireNextFrame` (or re-create) error means.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum Action {
    /// No new frame within the timeout: the desktop is idle (the CFR pacer repeats the last
    /// frame, as it does when WGC sends nothing).
    Idle,
    /// The duplication is gone (mode or resolution change, exclusive fullscreen switch, UAC
    /// secure desktop, another duplication): make a new one on the same device, with backoff.
    Recreate,
    /// The device is gone: the capture thread ends.
    Fatal,
}

pub(super) fn action_for(code: HRESULT) -> Action {
    match code {
        DXGI_ERROR_WAIT_TIMEOUT => Action::Idle,
        DXGI_ERROR_DEVICE_REMOVED | DXGI_ERROR_DEVICE_RESET | DXGI_ERROR_DEVICE_HUNG | DXGI_ERROR_DRIVER_INTERNAL_ERROR => Action::Fatal,
        DXGI_ERROR_ACCESS_LOST | DXGI_ERROR_ACCESS_DENIED | E_ACCESSDENIED => Action::Recreate,
        _ => Action::Recreate,
    }
}

/// Wait before re-create attempt `attempt` (0-based): 50 ms doubling, at most 2 s.
pub(super) fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(50u64.saturating_mul(1u64 << attempt.min(6)).min(2000))
}

/// QPC ticks (DXGI `LastPresentTime`) in the 100 ns units WGC timestamps use (both run on
/// QPC, so the timelines line up across a WGC <-> DDA capture restart).
pub(super) fn qpc_to_100ns(ticks: i64, frequency: i64) -> i64 {
    if frequency <= 0 {
        return 0;
    }
    (i128::from(ticks) * 10_000_000 / i128::from(frequency)) as i64
}

/// Whether a duplicated frame carries a new desktop image: a mouse-only update has
/// `LastPresentTime == 0`. The first frame after a (re)create (sent only once something on
/// screen changes) is the whole current desktop and is always taken.
pub(super) fn has_new_image(last_present_time: i64, accumulated_frames: u32, first: bool) -> bool {
    first || (last_present_time != 0 && accumulated_frames > 0)
}

#[cfg(test)]
#[path = "dda_logic_tests.rs"]
mod tests;
