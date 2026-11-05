//! Does an unknown fullscreen window render like a game? Pure logic over the
//! present counts `render_probe` collects for one process while the
//! candidate waits for its start debounce.
//!
//! Evidence (2026-11-05, `.planning/DETECT_RENDER.md`):
//! - VALORANT with Recall's island up, PresentMon 2026-11-01 (336 s): 244,629
//!   frames `Hardware: Independent Flip`, 12 `Hardware Composed`, and 39,652
//!   `Composed: Flip` in the stretches where Rohan had left the game window;
//!   every one of its 284,293 DXGI presents had `DXGI_PRESENT_ALLOW_TEARING`.
//! - Every app on the desktop in a 10 s system-wide trace (Recall's UI,
//!   DWM, steady-mic, Brave, Discord): DWM-composed only, 0 independent-flip
//!   tokens out of 5,055.
//! - Exclusive fullscreen is the kernel's legacy flip (`Flip_Info` on the
//!   presenting thread), as PresentMon classifies it.
//!
//! So a game is: legacy flips, or mostly independent-flip frames, or mostly
//! tearing presents. Video players present through DWM or through a decode
//! swap chain (`PresentMultiplaneOverlay`); slideshows and chat apps present
//! through DWM or not at all. Rate is not used: a 60 fps video and a game
//! capped at 60 on a 60 Hz laptop look alike.

use std::time::{Duration, Instant};

/// How long a candidate may be watched before the call is made on what was seen.
pub const MAX_WATCH: Duration = Duration::from_millis(1_500);
/// While unsure, ask again this often.
pub const ASK_EVERY: Duration = Duration::from_millis(100);
/// Frames a signal needs before it counts (3 frames = 100 ms at 30 fps).
pub const MIN_FRAMES: u32 = 3;
/// A window rejected after showing frames (composed, video) is watched again
/// after this, doubling up to [`RECHECK_MAX`]: 2, 4, 8, 16, 30, 30 s.
pub const RECHECK_FIRST: Duration = Duration::from_secs(2);
pub const RECHECK_MAX: Duration = Duration::from_secs(30);
/// A window that showed no frames at all (a loading screen, a game still
/// starting) is inconclusive: watched again after this every time, without
/// advancing the backoff above.
pub const RECHECK_NO_FRAMES: Duration = Duration::from_secs(2);

/// One process's presents since the probe started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// DXGI / D3D9 `Present` calls (occlusion tests excluded).
    pub runtime_presents: u32,
    /// Of those, DXGI presents with `DXGI_PRESENT_ALLOW_TEARING`.
    pub tearing_presents: u32,
    /// DXGI `PresentMultiplaneOverlay`: decode swap chains (video).
    pub video_presents: u32,
    /// Kernel presents (any API, including Vulkan and OpenGL).
    pub kernel_presents: u32,
    /// Kernel legacy flips: exclusive fullscreen.
    pub legacy_flips: u32,
    /// DWM put one of the process's frames on screen by independent flip.
    pub independent_frames: u32,
    /// DWM composed one of the process's frames.
    pub composed_frames: u32,
}

impl RenderStats {
    fn any(&self) -> bool {
        self.runtime_presents + self.kernel_presents + self.legacy_flips + self.independent_frames + self.composed_frames > 0
    }
}

/// Why a verdict was reached (the log line names it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    ExclusiveFullscreen,
    IndependentFlip,
    Tearing,
    VideoSwapChain,
    Composed,
    NoFrames,
}

impl Why {
    pub fn as_str(self) -> &'static str {
        match self {
            Why::ExclusiveFullscreen => "exclusive fullscreen",
            Why::IndependentFlip => "independent flip",
            Why::Tearing => "tearing presents",
            Why::VideoSwapChain => "video swap chain",
            Why::Composed => "composed by DWM",
            Why::NoFrames => "no frames",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Game(Why),
    NotGame(Why),
    /// Not enough seen yet.
    Unsure,
}

/// The verdict for `s`. `last` = the watch is over: decide on what was seen.
pub fn judge(s: &RenderStats, last: bool) -> Verdict {
    if s.legacy_flips >= MIN_FRAMES {
        return Verdict::Game(Why::ExclusiveFullscreen);
    }
    if s.video_presents >= MIN_FRAMES && s.video_presents * 2 >= s.runtime_presents {
        return Verdict::NotGame(Why::VideoSwapChain);
    }
    let tokens = s.independent_frames + s.composed_frames;
    if s.independent_frames >= MIN_FRAMES && s.independent_frames * 2 >= tokens {
        return Verdict::Game(Why::IndependentFlip);
    }
    if s.tearing_presents >= MIN_FRAMES && s.tearing_presents * 2 >= s.runtime_presents {
        return Verdict::Game(Why::Tearing);
    }
    match (last, s.any()) {
        (false, _) => Verdict::Unsure,
        (true, true) => Verdict::NotGame(Why::Composed),
        (true, false) => Verdict::NotGame(Why::NoFrames),
    }
}

/// What the start debounce does with a candidate that needs the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Commit,
    /// Ask again at this time.
    Hold(Instant),
    Reject(Why),
}

/// The probe's state for the candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watch {
    /// No trace rights, or the session failed: today's rule (fullscreen = game).
    Unavailable,
    Watching { since: Instant, stats: RenderStats },
}

pub fn gate(watch: Watch, now: Instant) -> (Gate, Verdict) {
    let Watch::Watching { since, stats } = watch else { return (Gate::Commit, Verdict::Unsure) };
    let end = since + MAX_WATCH;
    let verdict = judge(&stats, now >= end);
    let gate = match verdict {
        Verdict::Game(_) => Gate::Commit,
        Verdict::NotGame(why) => Gate::Reject(why),
        Verdict::Unsure => Gate::Hold((now + ASK_EVERY).min(end)),
    };
    (gate, verdict)
}

/// Windows judged not a game, so they are not watched again at once. One
/// entry: the foreground holds one window at a time.
#[derive(Debug, Default)]
pub struct Rejected {
    last: Option<Entry>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    pid: u32,
    hwnd: isize,
    until: Instant,
    /// The last frames-seen wait (zero: none yet), the base of the next one.
    wait: Duration,
}

impl Rejected {
    /// `(pid, hwnd)` was judged not a game (`why`) at `now`.
    pub fn note(&mut self, pid: u32, hwnd: isize, why: Why, now: Instant) {
        let level = match self.last {
            Some(e) if e.pid == pid && e.hwnd == hwnd => e.wait,
            _ => Duration::ZERO,
        };
        let (hold, wait) = if why == Why::NoFrames {
            (RECHECK_NO_FRAMES, level)
        } else {
            let next = if level.is_zero() { RECHECK_FIRST } else { (level * 2).min(RECHECK_MAX) };
            (next, next)
        };
        self.last = Some(Entry { pid, hwnd, until: now + hold, wait });
    }

    /// The window is still inside its wait.
    pub fn holds(&self, pid: u32, hwnd: isize, now: Instant) -> bool {
        self.last.is_some_and(|e| e.pid == pid && e.hwnd == hwnd && now < e.until)
    }

    /// When the held window may be watched again.
    pub fn until(&self) -> Option<Instant> {
        self.last.map(|e| e.until)
    }
}

#[cfg(test)]
#[path = "render_verdict_tests.rs"]
mod tests;
