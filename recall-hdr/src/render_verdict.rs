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

use std::collections::HashSet;
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
/// starting) is inconclusive: watched again after this, up to
/// [`NO_FRAMES_FAST`] times in a row (about 20 s). After that it backs off
/// 4, 8, 16, 30 s, so a window that never presents is not traced ~75% of the
/// time. A new window, or frames seen, starts over at 2 s.
pub const RECHECK_NO_FRAMES: Duration = Duration::from_secs(2);
pub const NO_FRAMES_FAST: u32 = 10;

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
    /// No trace rights, or the session failed: no verdict, so not a game
    /// (official-proof ruling, 2026-11-07; no "fullscreen = game" fallback).
    Unavailable,
    Watching { since: Instant, stats: RenderStats },
}

pub fn gate(watch: Watch, now: Instant) -> (Gate, Verdict) {
    let Watch::Watching { since, stats } = watch else { return (Gate::Reject(Why::NoFrames), Verdict::Unsure) };
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
    /// The last backed-off wait (zero: none yet), the base of the next one.
    wait: Duration,
    /// No-frames rejections in a row.
    quiet: u32,
    /// The last verdict.
    why: Why,
    /// What the probe saw for it (the G4 notice asks for a steady rate).
    stats: RenderStats,
}

impl Rejected {
    /// `(pid, hwnd)` was judged not a game (`why`, on `stats`) at `now`.
    pub fn note(&mut self, pid: u32, hwnd: isize, why: Why, stats: RenderStats, now: Instant) {
        let (mut wait, mut quiet) = match self.last {
            Some(e) if e.pid == pid && e.hwnd == hwnd => (e.wait, e.quiet),
            _ => (Duration::ZERO, 0),
        };
        let hold = if why == Why::NoFrames {
            quiet += 1;
            if quiet <= NO_FRAMES_FAST {
                RECHECK_NO_FRAMES
            } else {
                wait = if wait < RECHECK_NO_FRAMES * 2 { RECHECK_NO_FRAMES * 2 } else { (wait * 2).min(RECHECK_MAX) };
                wait
            }
        } else {
            if quiet > 0 {
                // It started presenting: start over.
                (wait, quiet) = (Duration::ZERO, 0);
            }
            wait = if wait.is_zero() { RECHECK_FIRST } else { (wait * 2).min(RECHECK_MAX) };
            wait
        };
        self.last = Some(Entry { pid, hwnd, until: now + hold, wait, quiet, why, stats });
    }

    /// The window is still inside its wait.
    pub fn holds(&self, pid: u32, hwnd: isize, now: Instant) -> bool {
        self.last.is_some_and(|e| e.pid == pid && e.hwnd == hwnd && now < e.until)
    }

    /// The window's last verdict was "composed by DWM" (inside its wait or
    /// not): what the probe saw, for the G4 notice.
    pub fn composed(&self, pid: u32, hwnd: isize) -> Option<RenderStats> {
        self.last.filter(|e| e.pid == pid && e.hwnd == hwnd && e.why == Why::Composed).map(|e| e.stats)
    }

    /// The window's last verdict and what the probe saw (inside its wait or
    /// not): the private-list notice (`record_private_watch`).
    pub fn verdict(&self, pid: u32, hwnd: isize) -> Option<(Why, RenderStats)> {
        self.last.filter(|e| e.pid == pid && e.hwnd == hwnd).map(|e| (e.why, e.stats))
    }

    /// When the held window may be watched again.
    pub fn until(&self) -> Option<Instant> {
        self.last.map(|e| e.until)
    }
}

/// How long a fullscreen window last judged "composed by DWM" must stay in
/// front before the player is told it is not recorded (G4).
pub const COMPOSED_NOTICE_AFTER: Duration = Duration::from_secs(20);

/// G4: a borderless game DWM composes (D3D9, Vulkan without independent
/// flip, an overlay on top) is rejected on every recheck. Once such a window
/// has stayed in front for more than [`COMPOSED_NOTICE_AFTER`], its exe is
/// named once per run, so the player can add it as a game.
#[derive(Debug, Default)]
pub struct ComposedNotice {
    /// (pid, hwnd) in front with a composed verdict, and since when.
    streak: Option<(u32, isize, Instant)>,
    /// A watch in the streak saw a game-like rate (`record_composed`).
    steady: bool,
    /// Exes already named this run (lower case).
    shown: HashSet<String>,
}

impl ComposedNotice {
    /// One foreground observation: `front` = (pid, hwnd, exe) when the
    /// foreground is a fullscreen window whose last verdict was composed and
    /// that is not recorded, else `None`. Returns the exe to name, once.
    pub fn observe(&mut self, front: Option<(u32, isize, &str)>, now: Instant) -> Option<String> {
        self.observe_steady(front.map(|f| (f, true)), now)
    }

    /// `exe` was already named this run.
    pub fn named(&self, exe: &str) -> bool {
        self.shown.contains(&exe.to_ascii_lowercase())
    }

    /// [`Self::observe`] where each composed observation says whether its
    /// watch saw a steady rate: the clock keeps running through slow
    /// rechecks, and the notice fires only if some watch in the streak was
    /// steady (R3-detect-1).
    pub fn observe_steady(&mut self, front: Option<((u32, isize, &str), bool)>, now: Instant) -> Option<String> {
        let Some(((pid, hwnd, exe), steady)) = front else {
            self.streak = None;
            return None;
        };
        let since = match self.streak {
            Some((p, h, since)) if (p, h) == (pid, hwnd) => since,
            _ => {
                self.streak = Some((pid, hwnd, now));
                self.steady = false;
                now
            }
        };
        self.steady |= steady;
        let due = self.steady && now.duration_since(since) > COMPOSED_NOTICE_AFTER;
        (due && self.shown.insert(exe.to_ascii_lowercase())).then(|| exe.to_string())
    }
}

#[cfg(test)]
#[path = "render_verdict_tests.rs"]
mod tests;
