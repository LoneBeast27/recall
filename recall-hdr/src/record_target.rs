//! Capture-target selection for the unified record mode. Pure logic, no Win32:
//! the pump feeds [`WatchEvent`]s in and reads the target out (what the engine
//! then does with it lives in `record_engine`).
//!
//! - **Classification** ([`classify_event`]): a target is a foreground window
//!   that is fullscreen/borderless on its own monitor, an exe in a Steam library
//!   (windowed games), or an exe with an `hdr_game|sdr_game` override; never a
//!   denied stem (the rules deny list,
//!   Recall's own processes, explorer, browsers, media players, overlays,
//!   launchers) or an exe with a `none` override. A window that is a
//!   candidate by the fullscreen rule alone must also render like a game
//!   before it commits (`record_render`, asked through [`Admit`]).
//! - **Session** ([`TargetTracker`]): the same rules as the HDR `Reconciler`
//!   session, applied to capture. With nothing recorded (no session, or the
//!   engine lingering after a game exited) a candidate commits once it holds
//!   the foreground for [`START_DEBOUNCE`] (300 ms): the first game starts
//!   recording almost at once. A *different* game (another process) replaces a
//!   live session only after [`TARGET_DEBOUNCE`] (2 s), so a brief alt-tab to
//!   another game does not restart the engine; a new window of the session's
//!   own process (a fullscreen mode switch) needs only [`START_DEBOUNCE`]. The
//!   deadline follows the session: if the session ends while a candidate
//!   waits, the short debounce applies from when that candidate was first
//!   seen. A non-target foreground (alt-tab) never ends the session; the
//!   session ends when its process exits or its window is destroyed.

use std::time::{Duration, Instant};

use crate::config::Config;
use crate::rules;
use crate::types::RuleKind;
use crate::watcher::WatchEvent;

/// How long a different game must hold the foreground before it replaces a
/// live session (alt-tab protection).
pub const TARGET_DEBOUNCE: Duration = Duration::from_secs(2);
/// How long a candidate must hold the foreground when nothing is recorded, or
/// when it is a new window of the session's own process.
pub const START_DEBOUNCE: Duration = Duration::from_millis(300);
/// Source name of the idle monitor capture.
pub const DESKTOP_NAME: &str = "Desktop";

/// Stems that are never a capture target even when fullscreen (on top of
/// [`rules::is_denied`]): Recall's own processes, browsers, media players,
/// full-screen overlays (which would otherwise steal the target from the game
/// underneath and restart the engine), launchers, remote-desktop clients, and
/// shell surfaces. Steam's `streaming_client` (Remote Play) is deliberately
/// absent: a streamed game is recorded as a game.
const TARGET_DENY_STEMS: &[&str] = &[
    "recall-native",
    "recall-hub",
    "recall-overlay",
    "nvidia overlay",
    "nvidia share",
    "gamebar",
    "gamebarftserver",
    "overwolf",
    "steam",
    "steamwebhelper",
    "epicgameslauncher",
    "riotclientux",
    "riotclientservices",
    "battle.net",
    "eadesktop",
    "galaxyclient",
    "upc",
    "uplay",
    "ubisoftconnect",
    "origin",
    "xboxpcapp",
    "xboxapp",
    "gamingservicesui",
    "amazongames",
    "amazon games ui",
    "playnite.fullscreenapp",
    "playnite.desktopapp",
    "shellhost",
    "brave",
    "chrome",
    "chromium",
    "msedge",
    "msedgewebview2",
    "firefox",
    "librewolf",
    "waterfox",
    "opera",
    "opera_gx",
    "vivaldi",
    "arc",
    "zen",
    "iexplore",
    "vlc",
    "mpv",
    "mpc-hc",
    "mpc-hc64",
    "potplayermini64",
    "wmplayer",
    "lockapp",
    "screenclippinghost",
    "snippingtool",
    // Remote desktop / remote viewer clients: a fullscreen session is a desktop,
    // not a game (msrdc.exe was recorded as "Remote Desktop", 2026-11-04).
    "mstsc",
    "msrdc",
    "msrdcw",
    "rdclient",
    "rdclient.windows",
    "windows365",
    "windowsapp",
    "rdcman",
    "vmconnect",
    "vmware-remotemks",
    "virtualboxvm",
    "mremoteng",
    "royalts",
    "remotedesktopmanager",
    "remotedesktopmanager64",
    "anydesk",
    "teamviewer",
    "rustdesk",
    "parsecd",
    "moonlight",
    "remoting_desktop",
    "remoting_host",
    "vncviewer",
    "tvnviewer",
    "nomachine",
    "nxplayer",
    "splashtop",
    "strwinclt",
    // Steam-library apps that are not games (the Steam-library rule would take them).
    "wallpaper32",
    "wallpaper64",
    "ui32",
];

/// A window the engine can capture. `exe` is the image file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub hwnd: isize,
    pub pid: u32,
    pub name: String,
    pub exe: String,
}

impl Target {
    pub fn from_event(ev: &WatchEvent) -> Target {
        Target {
            hwnd: ev.hwnd,
            pid: ev.pid,
            name: display_name(&ev.title, &ev.exe_stem),
            exe: ev.exe_name.clone(),
        }
    }

    fn same_window(&self, other: &Target) -> bool {
        self.pid == other.pid && self.hwnd == other.hwnd
    }
}

/// Collapse whitespace, drop control characters, cap at 64 chars.
pub fn sanitize(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(64).collect()
}

/// Source name: the sanitized window title, else the exe stem, else "Game".
pub fn display_name(title: &str, exe_stem: &str) -> String {
    let t = sanitize(title);
    if !t.is_empty() {
        return t;
    }
    let s = sanitize(exe_stem);
    if s.is_empty() { "Game".into() } else { s }
}

/// Whether an exe stem is a capture target under `cfg`. `looks_like_game` =
/// fullscreen on its monitor, or the exe lives in a Steam library.
pub fn is_target(stem: &str, looks_like_game: bool, cfg: &Config) -> bool {
    let stem = strip_exe(stem.trim());
    if stem.is_empty()
        || rules::is_denied(stem)
        || TARGET_DENY_STEMS.iter().any(|d| d.eq_ignore_ascii_case(stem))
    {
        return false;
    }
    match rules::override_kind(cfg, stem) {
        Some(RuleKind::None) => false,
        Some(RuleKind::HdrGame | RuleKind::SdrGame) => true,
        None => looks_like_game,
    }
}

/// `"msrdc.exe"` -> `"msrdc"` (case-insensitive); a bare stem is unchanged.
fn strip_exe(s: &str) -> &str {
    match s.len().checked_sub(4) {
        Some(i) if s.is_char_boundary(i) && s[i..].eq_ignore_ascii_case(".exe") => &s[..i],
        _ => s,
    }
}

/// `Some(target)` if this foreground event is a capture target.
pub fn classify_event(ev: &WatchEvent, cfg: &Config) -> Option<Target> {
    is_target(&ev.exe_stem, ev.fullscreen || ev.steam_game, cfg).then(|| Target::from_event(ev))
}

/// The debounced capture session (see the module docs).
pub struct TargetTracker {
    /// Replacing a live session with a different game.
    debounce: Duration,
    /// Starting a session from nothing, or a new window of the session's process.
    start: Duration,
    current: Option<Target>,
    /// A foreground observation and when it was first seen; `None` inside = a
    /// non-target foreground (it cancels a pending game, never the session).
    pending: Option<(Option<Target>, Instant)>,
    /// How long the last committed candidate waited (first seen -> commit).
    last_wait: Option<Duration>,
    /// The render check asked to decide the pending candidate later.
    held: Option<Instant>,
}

/// What the render check says about a candidate that fell due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    Commit,
    /// Ask again at this time; the candidate stays pending.
    Hold(Instant),
    /// Not a game: drop the candidate (the session, if any, stays).
    Reject,
}

impl TargetTracker {
    pub fn new(debounce: Duration, start: Duration) -> Self {
        TargetTracker { debounce, start, current: None, pending: None, last_wait: None, held: None }
    }

    pub fn current(&self) -> Option<&Target> {
        self.current.as_ref()
    }

    /// The hold a candidate needs against the current session.
    fn hold_for(&self, candidate: &Option<Target>) -> Duration {
        match (candidate, &self.current) {
            (Some(c), Some(cur)) if c.pid != cur.pid => self.debounce,
            _ => self.start,
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let due = self.pending.as_ref().map(|(c, seen)| *seen + self.hold_for(c))?;
        Some(self.held.map_or(due, |held| held.max(due)))
    }

    /// The pending candidate, if it is a window (not an alt-tab away).
    pub fn pending(&self) -> Option<&Target> {
        self.pending.as_ref().and_then(|(c, _)| c.as_ref())
    }

    /// A foreground change was observed (`None` = not a target). Seeing the
    /// pending candidate again keeps its first-seen time.
    pub fn on_foreground(&mut self, candidate: Option<Target>, now: Instant) {
        match (&candidate, &self.current, &self.pending) {
            // Re-focusing the live target: nothing pending, nothing to do.
            (Some(c), Some(cur), _) if c.same_window(cur) => self.pending = None,
            (Some(c), _, Some((Some(p), _))) if c.same_window(p) => {}
            (None, _, Some((None, _))) => {}
            _ => {
                self.pending = Some((candidate, now));
                self.held = None;
            }
        }
    }

    /// Commit a settled candidate. Returns `true` when the target changed.
    pub fn poll(&mut self, now: Instant) -> bool {
        self.poll_admit(now, |_| Admit::Commit)
    }

    /// [`Self::poll`], asking `admit` about a due candidate before it commits.
    pub fn poll_admit(&mut self, now: Instant, admit: impl FnOnce(&Target) -> Admit) -> bool {
        let due = self.next_deadline().is_some_and(|d| now >= d);
        if !due {
            return false;
        }
        let Some((candidate, seen)) = self.pending.take() else { return false };
        self.held = None;
        match candidate {
            // Alt-tab / desktop: the session persists.
            None => false,
            Some(c) if self.current.as_ref().is_some_and(|cur| cur.same_window(&c)) => false,
            Some(c) => match admit(&c) {
                Admit::Commit => {
                    self.current = Some(c);
                    self.last_wait = Some(now.duration_since(seen));
                    true
                }
                Admit::Hold(at) => {
                    self.pending = Some((Some(c), seen));
                    self.held = Some(at);
                    false
                }
                Admit::Reject => false,
            },
        }
    }

    /// How long the last committed target waited from first seen to commit
    /// (taken once, for the retarget log line).
    pub fn take_last_wait(&mut self) -> Option<Duration> {
        self.last_wait.take()
    }

    /// The process `pid` exited: end its session (and drop a pending candidate
    /// for it). Returns `true` when the target changed.
    pub fn on_process_exit(&mut self, pid: u32) -> bool {
        if matches!(&self.pending, Some((Some(c), _)) if c.pid == pid) {
            self.pending = None;
            self.held = None;
        }
        if self.current.as_ref().is_some_and(|c| c.pid == pid) {
            self.current = None;
            return true;
        }
        false
    }

    /// The target's window was destroyed while its process lives on (e.g. the
    /// game dropped back to an in-process launcher): end the session so the
    /// engine is not left capturing a dead window. Returns `true` on change.
    pub fn on_window_gone(&mut self, hwnd: isize) -> bool {
        if self.current.as_ref().is_some_and(|c| c.hwnd == hwnd) {
            self.current = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
#[path = "record_target_tests.rs"]
mod tests;
