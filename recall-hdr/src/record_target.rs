//! Capture-target selection for the unified record mode. Pure logic, no Win32:
//! the pump feeds [`WatchEvent`]s in and reads the target out (what the engine
//! then does with it lives in `record_engine`).
//!
//! - **Classification** ([`classify_event`]): a target is a foreground window
//!   that is fullscreen/borderless on its own monitor, an exe in a Steam library
//!   (windowed games), or an exe with an `hdr_game|sdr_game` override; never a
//!   denied stem (the rules deny list,
//!   Recall's own processes, explorer, browsers, media players, overlays,
//!   launchers) or an exe with a `none` override.
//! - **Session** ([`TargetTracker`]): the same rules as the HDR `Reconciler`
//!   session, applied to capture: a candidate commits only after it stays
//!   foreground for [`TARGET_DEBOUNCE`]; a non-target foreground (alt-tab) never
//!   ends the session; a *different* game that settles replaces it; the
//!   session ends only when its process exits.

use std::time::{Duration, Instant};

use crate::config::Config;
use crate::rules;
use crate::types::RuleKind;
use crate::watcher::WatchEvent;

/// How long a candidate must hold the foreground before it becomes the target.
pub const TARGET_DEBOUNCE: Duration = Duration::from_secs(2);
/// Source name of the idle monitor capture.
pub const DESKTOP_NAME: &str = "Desktop";

/// Stems that are never a capture target even when fullscreen (on top of
/// [`rules::is_denied`]): Recall's own processes, browsers, media players,
/// full-screen overlays (which would otherwise steal the target from the game
/// underneath and restart the engine), launchers, and shell surfaces.
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
    let stem = stem.trim();
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

/// `Some(target)` if this foreground event is a capture target.
pub fn classify_event(ev: &WatchEvent, cfg: &Config) -> Option<Target> {
    is_target(&ev.exe_stem, ev.fullscreen || ev.steam_game, cfg).then(|| Target::from_event(ev))
}

/// The debounced capture session (see the module docs).
pub struct TargetTracker {
    debounce: Duration,
    current: Option<Target>,
    /// A foreground observation awaiting its deadline; `None` inside = a
    /// non-target foreground (it cancels a pending game, never the session).
    pending: Option<(Option<Target>, Instant)>,
}

impl TargetTracker {
    pub fn new(debounce: Duration) -> Self {
        TargetTracker { debounce, current: None, pending: None }
    }

    pub fn current(&self) -> Option<&Target> {
        self.current.as_ref()
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|(_, d)| *d)
    }

    /// A foreground change was observed (`None` = not a target).
    pub fn on_foreground(&mut self, candidate: Option<Target>, now: Instant) {
        match (&candidate, &self.current) {
            // Re-focusing the live target: nothing pending, nothing to do.
            (Some(c), Some(cur)) if c.same_window(cur) => self.pending = None,
            _ => self.pending = Some((candidate, now + self.debounce)),
        }
    }

    /// Commit a settled candidate. Returns `true` when the target changed.
    pub fn poll(&mut self, now: Instant) -> bool {
        let due = matches!(&self.pending, Some((_, deadline)) if now >= *deadline);
        if !due {
            return false;
        }
        let Some((candidate, _)) = self.pending.take() else { return false };
        match candidate {
            // Alt-tab / desktop: the session persists.
            None => false,
            Some(c) if self.current.as_ref().is_some_and(|cur| cur.same_window(&c)) => false,
            Some(c) => {
                self.current = Some(c);
                true
            }
        }
    }

    /// The process `pid` exited: end its session (and drop a pending candidate
    /// for it). Returns `true` when the target changed.
    pub fn on_process_exit(&mut self, pid: u32) -> bool {
        if matches!(&self.pending, Some((Some(c), _)) if c.pid == pid) {
            self.pending = None;
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
mod tests {
    use super::*;
    use crate::config::Override;

    fn ev(stem: &str, title: &str, pid: u32, hwnd: isize, fullscreen: bool) -> WatchEvent {
        WatchEvent {
            pid,
            exe_stem: stem.into(),
            exe_name: format!("{stem}.exe"),
            title: title.into(),
            fullscreen,
            fullscreen_on_target: fullscreen,
            steam_game: false,
            hwnd,
        }
    }

    fn steam_ev(stem: &str) -> WatchEvent {
        WatchEvent { steam_game: true, ..ev(stem, stem, 7, 0x70, false) }
    }

    fn target(pid: u32, hwnd: isize, name: &str) -> Target {
        Target { hwnd, pid, name: name.into(), exe: format!("{name}.exe") }
    }

    fn user_cfg() -> Config {
        let mut cfg = Config::default();
        cfg.overrides.push(Override { exe: "brave".into(), kind: RuleKind::None, clamp: None });
        cfg.overrides.push(Override { exe: "windowedgame".into(), kind: RuleKind::SdrGame, clamp: None });
        cfg.overrides.push(Override { exe: "spotify".into(), kind: RuleKind::None, clamp: None });
        cfg
    }

    #[test]
    fn fullscreen_unknown_game_is_a_target_named_by_title() {
        let cfg = user_cfg();
        let t = classify_event(&ev("VALORANT-Win64-Shipping", "VALORANT  ", 10, 0x10, true), &cfg).unwrap();
        assert_eq!(t.name, "VALORANT");
        assert_eq!(t.exe, "VALORANT-Win64-Shipping.exe");
        assert_eq!((t.pid, t.hwnd), (10, 0x10));
        let untitled = classify_event(&ev("RocketLeague", "", 11, 0x11, true), &cfg).unwrap();
        assert_eq!(untitled.name, "RocketLeague", "falls back to the exe stem");
    }

    #[test]
    fn windowed_unknown_is_not_a_target_but_an_override_is() {
        let cfg = user_cfg();
        assert!(classify_event(&ev("notepad", "x", 1, 1, false), &cfg).is_none());
        assert!(classify_event(&ev("WindowedGame", "x", 1, 1, false), &cfg).is_some());
    }

    #[test]
    fn windowed_steam_library_game_is_a_target_unless_denied() {
        let cfg = user_cfg();
        assert!(classify_event(&steam_ev("sobosuba"), &cfg).is_some(), "maximized windowed Steam game");
        for stem in ["wallpaper64", "ui32", "brave"] {
            assert!(classify_event(&steam_ev(stem), &cfg).is_none(), "{stem} in a Steam library is not a game");
        }
        let mut off = user_cfg();
        off.overrides.push(Override { exe: "sobosuba".into(), kind: RuleKind::None, clamp: None });
        assert!(classify_event(&steam_ev("sobosuba"), &off).is_none(), "a none override still wins");
    }

    #[test]
    fn denied_and_none_override_stems_are_never_targets() {
        let cfg = user_cfg();
        for stem in ["explorer", "recall", "recall-hdr", "recall-native", "recall-overlay", "chrome", "msedge", "firefox", "brave", "spotify", "vlc", "NVIDIA Overlay", "GameBar", "steamwebhelper"] {
            assert!(!is_target(stem, true, &cfg), "{stem} fullscreen must not be recorded");
        }
        assert!(!is_target("  ", true, &cfg));
    }

    #[test]
    fn default_game_class_none_still_records_fullscreen_games() {
        // HDR rules use default_game_class; capture does not.
        let mut cfg = user_cfg();
        cfg.behavior.default_game_class = RuleKind::None;
        assert!(is_target("eldenring", true, &cfg));
    }

    #[test]
    fn sanitize_collapses_and_caps() {
        assert_eq!(sanitize("  Elden\tRing\u{7}  "), "Elden Ring");
        assert_eq!(sanitize(&"x".repeat(100)).chars().count(), 64);
        assert_eq!(display_name("", ""), "Game");
    }

    #[test]
    fn candidate_commits_only_after_debounce() {
        let t0 = Instant::now();
        let mut tr = TargetTracker::new(TARGET_DEBOUNCE);
        tr.on_foreground(Some(target(1, 1, "A")), t0);
        assert!(!tr.poll(t0 + Duration::from_millis(1999)));
        assert!(tr.current().is_none());
        assert!(tr.poll(t0 + TARGET_DEBOUNCE));
        assert_eq!(tr.current().unwrap().pid, 1);
    }

    #[test]
    fn brief_focus_of_another_game_does_not_retarget() {
        let t0 = Instant::now();
        let mut tr = TargetTracker::new(TARGET_DEBOUNCE);
        tr.on_foreground(Some(target(1, 1, "A")), t0);
        tr.poll(t0 + TARGET_DEBOUNCE);
        // B is foreground for 1 s, then the desktop: B never commits.
        tr.on_foreground(Some(target(2, 2, "B")), t0 + Duration::from_secs(5));
        tr.on_foreground(None, t0 + Duration::from_secs(6));
        assert!(!tr.poll(t0 + Duration::from_secs(20)));
        assert_eq!(tr.current().unwrap().pid, 1);
    }

    #[test]
    fn alt_tab_keeps_target_while_its_process_lives() {
        let t0 = Instant::now();
        let mut tr = TargetTracker::new(TARGET_DEBOUNCE);
        tr.on_foreground(Some(target(1, 1, "A")), t0);
        tr.poll(t0 + TARGET_DEBOUNCE);
        tr.on_foreground(None, t0 + Duration::from_secs(10)); // desktop for a minute
        assert!(!tr.poll(t0 + Duration::from_secs(70)));
        assert_eq!(tr.current().unwrap().pid, 1, "target persists off-foreground");
        // Back to the game: no change, nothing pending.
        tr.on_foreground(Some(target(1, 1, "A")), t0 + Duration::from_secs(71));
        assert!(tr.next_deadline().is_none());
    }

    #[test]
    fn a_settled_different_game_replaces_the_target() {
        let t0 = Instant::now();
        let mut tr = TargetTracker::new(TARGET_DEBOUNCE);
        tr.on_foreground(Some(target(1, 1, "A")), t0);
        tr.poll(t0 + TARGET_DEBOUNCE);
        tr.on_foreground(Some(target(2, 2, "B")), t0 + Duration::from_secs(5));
        assert!(tr.poll(t0 + Duration::from_secs(7)));
        assert_eq!(tr.current().unwrap().pid, 2);
        // Same process, new window (fullscreen mode switch) also retargets.
        tr.on_foreground(Some(target(2, 3, "B")), t0 + Duration::from_secs(9));
        assert!(tr.poll(t0 + Duration::from_secs(11)));
        assert_eq!(tr.current().unwrap().hwnd, 3);
    }

    #[test]
    fn process_exit_clears_target_and_pending() {
        let t0 = Instant::now();
        let mut tr = TargetTracker::new(TARGET_DEBOUNCE);
        tr.on_foreground(Some(target(1, 1, "A")), t0);
        tr.poll(t0 + TARGET_DEBOUNCE);
        assert!(!tr.on_process_exit(99), "unrelated exit ignored");
        assert!(tr.on_process_exit(1));
        assert!(tr.current().is_none());
        // A candidate whose process dies before committing never commits.
        tr.on_foreground(Some(target(5, 5, "C")), t0 + Duration::from_secs(3));
        tr.on_process_exit(5);
        assert!(!tr.poll(t0 + Duration::from_secs(10)));
        assert!(tr.current().is_none());
    }

    #[test]
    fn destroyed_target_window_ends_the_session() {
        let t0 = Instant::now();
        let mut tr = TargetTracker::new(TARGET_DEBOUNCE);
        tr.on_foreground(Some(target(1, 0x10, "A")), t0);
        tr.poll(t0 + TARGET_DEBOUNCE);
        assert!(!tr.on_window_gone(0x99), "another window is irrelevant");
        assert!(tr.on_window_gone(0x10));
        assert!(tr.current().is_none());
    }
}
