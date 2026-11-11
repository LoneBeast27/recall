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

    /// How long to wait before the next acquire, so acquires run at the capture rate too
    /// (frames above it accumulate in the duplication instead of being acquired and dropped).
    pub(super) fn until_due(&self, now: Instant) -> Duration {
        self.next_due.map_or(Duration::ZERO, |due| due.saturating_duration_since(now))
    }
}

/// Re-creates that keep failing this long (output gone after a replug, a monitor that stays
/// asleep, an adapter change) end Desktop Duplication for the run: capture restarts on WGC.
pub(super) const GIVE_UP: Duration = Duration::from_secs(5);

pub(super) fn should_give_up(failing_since: Instant, now: Instant) -> bool {
    now.saturating_duration_since(failing_since) > GIVE_UP
}

/// Whether a capture start uses Desktop Duplication: only on `RECALL_CAPTURE=dda` (opt-in
/// until checked live), and not after it failed earlier this run. Err is the WGC reason.
pub(crate) fn dda_wanted(env: Option<&str>, failed_earlier: bool) -> Result<(), &'static str> {
    match env {
        Some(v) if v.trim().eq_ignore_ascii_case("dda") => {
            if failed_earlier { Err("desktop duplication failed earlier this run") } else { Ok(()) }
        }
        Some(v) if v.trim().eq_ignore_ascii_case("wgc") => Err("RECALL_CAPTURE=wgc"),
        _ => Err("default; RECALL_CAPTURE=dda opts in to desktop duplication"),
    }
}

/// A screen rectangle (left/top inclusive, right/bottom exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// The window covers the whole monitor (borderless or fullscreen: it may overhang it).
/// A window style with no title bar (`WS_CAPTION`) and no sizing frame (`WS_THICKFRAME`): a
/// borderless or exclusive-fullscreen game. A maximized ordinary window overhangs its monitor
/// too (`covers` accepts that), but it has a caption: recording the monitor would show its
/// title bar and, with an auto-hide taskbar, the taskbar.
pub(super) fn frameless(style: u32) -> bool {
    const WS_CAPTION: u32 = 0x00C0_0000;
    const WS_THICKFRAME: u32 = 0x0004_0000;
    style & (WS_CAPTION | WS_THICKFRAME) == 0
}

pub(super) fn covers(window: Bounds, monitor: Bounds) -> bool {
    monitor.right > monitor.left
        && monitor.bottom > monitor.top
        && window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

/// How often a window-following capture looks at the game window and what sits over it.
pub(super) const FOLLOW_CHECK: Duration = Duration::from_millis(250);

/// A visible top-level window above the followed game in z-order (`dda_window::snapshot`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Above {
    pub rect: Bounds,
    /// DWM keeps it off screen (another virtual desktop, a suspended UWP app).
    pub cloaked: bool,
    /// `WS_EX_TOOLWINDOW`: tooltips, the taskbar, overlays.
    pub tool: bool,
    /// `WS_EX_LAYERED | WS_EX_TRANSPARENT`: a click-through overlay.
    pub click_through: bool,
    /// Never shows in a capture (`WDA_EXCLUDEFROMCAPTURE`: Recall's island and guide), or
    /// belongs to Recall's own process.
    pub excluded: bool,
    /// Belongs to the game's own process (a popup of the game).
    pub game: bool,
}

/// One look at the followed game window (`dda_window::snapshot`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct WindowState {
    pub exists: bool,
    pub iconic: bool,
    /// The monitor (`HMONITOR` value) the window is on; 0 when it is on none.
    pub monitor: usize,
    /// Frameless and covering that whole monitor (`covering_monitor`).
    pub covers: bool,
    /// The foreground window belongs to the game's process.
    pub foreground_is_game: bool,
    /// The captured monitor's rectangle.
    pub monitor_rect: Bounds,
    /// Visible windows above the game, nearest first (empty when the game is in front).
    pub above: Vec<Above>,
}

/// Whether the screen shows the user's desktop at all (`dda_desktop::now`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Desktop {
    /// The input desktop is the user's (false on the lock screen, UAC, Ctrl+Alt+Del).
    pub input_is_users: bool,
    pub display_on: bool,
}

impl Desktop {
    #[cfg(test)]
    pub(super) const USER: Self = Self { input_is_users: true, display_on: true };

    /// Duplication cannot work now and will once the user is back: never a reason to give up.
    pub(super) fn away(self) -> bool {
        !self.input_is_users || !self.display_on
    }
}

/// Why frames are held (the clip shows the last game frame, as WGC does on a minimized window).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hold {
    /// Another window sits over the game on its monitor (privacy: it is never recorded).
    Covered,
    Minimized,
    /// The window stopped covering its monitor (mode switch); restarts after `LEAVE_GRACE`.
    NotCovering,
    /// The window is gone; restarts after `LEAVE_GRACE`.
    Gone,
    /// Lock screen, UAC, Ctrl+Alt+Del or display off.
    Away,
}

impl Hold {
    pub(super) fn label(self) -> &'static str {
        match self {
            Hold::Covered => "another window covers the game",
            Hold::Minimized => "the game is minimized",
            Hold::NotCovering => "the game window does not cover its monitor",
            Hold::Gone => "the game window is gone",
            Hold::Away => "the user's desktop is not on screen",
        }
    }
}

/// What the capture does after a look at the game window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Follow {
    /// Frames go to the encoder.
    Deliver,
    Hold(Hold),
    /// Ask the engine for a new capture (`CaptureShared::restart`), with the reason.
    Restart(&'static str),
}

/// A window that leaves its monitor's full area (windowed mode, closed) for this long ends
/// the monitor capture: it restarts (on WGC window capture, or a new target).
pub(super) const LEAVE_GRACE: Duration = Duration::from_secs(2);

/// Follows the game window across looks (it times how long it has been off its monitor).
#[derive(Debug, Default)]
pub(super) struct Follower {
    leaving_since: Option<Instant>,
}

impl Follower {
    /// What to do after this look at the game window. Holds never restart: a covered or
    /// minimized game comes back on the same capture, so the encoder and replay buffer keep
    /// running. Only a move to another monitor restarts at once; a window that stays off its
    /// monitor's full area (or gone) restarts after `LEAVE_GRACE`.
    pub(super) fn decide(&mut self, state: &WindowState, captured: usize, desktop: Desktop, now: Instant) -> Follow {
        let look = look(state, captured, desktop);
        let Follow::Hold(why @ (Hold::NotCovering | Hold::Gone)) = look else {
            self.leaving_since = None;
            return look;
        };
        let since = *self.leaving_since.get_or_insert(now);
        if now.saturating_duration_since(since) < LEAVE_GRACE {
            return look;
        }
        Follow::Restart(if why == Hold::Gone { "the game window closed" } else { "the game window no longer covers its monitor" })
    }
}

/// A window above the game must overlap the monitor at least this much each way to hide
/// it (Explorer keeps 1x1 helper windows on top).
const MIN_OVERLAP: i32 = 8;

/// One look, with no memory of earlier ones.
fn look(state: &WindowState, captured: usize, desktop: Desktop) -> Follow {
    if desktop.away() {
        return Follow::Hold(Hold::Away);
    }
    if !state.exists {
        return Follow::Hold(Hold::Gone);
    }
    if state.iconic {
        return Follow::Hold(Hold::Minimized);
    }
    if state.monitor != 0 && state.monitor != captured {
        return Follow::Restart("the game window moved to another monitor");
    }
    if !state.covers || state.monitor == 0 {
        return Follow::Hold(Hold::NotCovering);
    }
    if !state.foreground_is_game && state.above.iter().any(|a| hides(a, state.monitor_rect)) {
        return Follow::Hold(Hold::Covered);
    }
    Follow::Deliver
}

/// A window above the game that shows on the captured monitor (so it would be recorded).
fn hides(above: &Above, monitor: Bounds) -> bool {
    if above.cloaked || above.tool || above.click_through || above.excluded || above.game {
        return false;
    }
    let w = above.rect.right.min(monitor.right) - above.rect.left.max(monitor.left);
    let h = above.rect.bottom.min(monitor.bottom) - above.rect.top.max(monitor.top);
    w >= MIN_OVERLAP && h >= MIN_OVERLAP
}

/// When re-creates started failing.
#[derive(Debug, Default)]
pub(super) struct FailClock {
    since: Option<Instant>,
}

impl FailClock {
    /// Before a re-create attempt: true = give up on Desktop Duplication. Time on the
    /// secure desktop or with the display off never counts (the count starts again when the
    /// user is back), so the lock screen, UAC or a sleeping monitor never ends DDA.
    pub(super) fn give_up(&mut self, now: Instant, desktop: Desktop) -> bool {
        if desktop.away() {
            self.since = None;
            return false;
        }
        should_give_up(*self.since.get_or_insert(now), now)
    }

    /// A re-create worked.
    pub(super) fn recovered(&mut self) {
        self.since = None;
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
    /// No new frame within the timeout: the desktop is idle (the last frame lasts until the
    /// next one in the variable-rate stream, as it does when WGC sends nothing).
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

#[cfg(test)]
#[path = "dda_follow_tests.rs"]
mod follow_tests;
