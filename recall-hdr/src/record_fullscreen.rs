//! Exclusive fullscreen hides the island: Windows composites no other window
//! over a game that owns the display (Rocket League in "Fullscreen"). While a
//! recorded game is in front, the controller reads Windows' own answer
//! (`SHQueryUserNotificationState` = `QUNS_RUNNING_D3D_FULL_SCREEN`, the check
//! the build gate uses) every [`CHECK_EVERY`]. Once a game has been seen in
//! exclusive fullscreen, the island tells the user once per game per run, as
//! soon as it can be seen again (the game left exclusive fullscreen: alt-tab,
//! its menus in a window, or it closed): switch the game to Borderless.

use std::collections::HashSet;
use std::time::{Duration, Instant};

pub const CHECK_EVERY: Duration = Duration::from_secs(2);

/// The game (exe, display name) the warning is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hidden {
    pub exe: String,
    pub name: String,
}

#[derive(Debug, Default)]
pub struct FullscreenWatch {
    /// Games already told about this run (lower-case exe).
    told: HashSet<String>,
    /// A game seen in exclusive fullscreen, not told yet.
    pending: Option<Hidden>,
    next: Option<Instant>,
}

impl FullscreenWatch {
    /// Whether a check is due now (the caller then reads the state and calls [`Self::observe`]).
    pub fn due(&mut self, now: Instant) -> bool {
        if self.next.is_some_and(|at| now < at) {
            return false;
        }
        self.next = Some(now + CHECK_EVERY);
        true
    }

    /// `game`: the recorded game, when it is in front (None: no game, or another
    /// window in front). `exclusive`: Windows reports exclusive fullscreen.
    /// Returns the game to warn about, once, when the island can show again.
    pub fn observe(&mut self, game: Option<Hidden>, exclusive: bool) -> Option<Hidden> {
        match (game, exclusive) {
            (Some(game), true) => {
                if !self.told.contains(&game.exe.to_ascii_lowercase()) {
                    self.pending = Some(game);
                }
                None
            }
            _ => {
                let hidden = self.pending.take()?;
                self.told.insert(hidden.exe.to_ascii_lowercase());
                Some(hidden)
            }
        }
    }
}

/// The game's name without a build suffix: "Rocket League (64-bit, DX11, Cooked)" → "Rocket League".
pub fn short_name(name: &str) -> &str {
    name.split(" (").next().unwrap_or(name).trim()
}

/// The `session` `notice` for a hidden island (shown even with notices off:
/// it is about the island itself).
pub fn notice_json(hidden: &Hidden) -> String {
    let detail = format!("{} runs in exclusive fullscreen. Set it to Borderless to see the island in game.", short_name(&hidden.name));
    crate::record_notice::shortcut_notice_json("Island hidden by fullscreen", &detail, true, true)
}

/// Windows' answer: a Direct3D app owns the display in exclusive fullscreen.
#[cfg(windows)]
pub fn exclusive_now() -> bool {
    use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_RUNNING_D3D_FULL_SCREEN};
    // SAFETY: plain query, no pointers kept.
    matches!(unsafe { SHQueryUserNotificationState() }, Ok(state) if state == QUNS_RUNNING_D3D_FULL_SCREEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rl() -> Option<Hidden> {
        Some(Hidden { exe: "RocketLeague.exe".into(), name: "Rocket League (64-bit, DX11, Cooked)".into() })
    }

    #[test]
    fn the_warning_waits_until_the_island_can_show_and_comes_once() {
        let mut w = FullscreenWatch::default();
        assert_eq!(w.observe(rl(), false), None, "borderless: nothing to say");
        assert_eq!(w.observe(rl(), true), None, "exclusive: the island cannot show yet");
        assert_eq!(w.observe(rl(), true), None);
        assert_eq!(w.observe(None, false), rl(), "alt-tab: now it can");
        assert_eq!(w.observe(rl(), true), None, "told once per game");
        assert_eq!(w.observe(None, false), None);
        let other = Some(Hidden { exe: "cs2.exe".into(), name: "Counter-Strike 2".into() });
        w.observe(other.clone(), true);
        assert_eq!(w.observe(other.clone(), false), other, "another game is told too");
    }

    #[test]
    fn checks_run_every_two_seconds() {
        let t0 = Instant::now();
        let mut w = FullscreenWatch::default();
        assert!(w.due(t0));
        assert!(!w.due(t0 + Duration::from_millis(1999)));
        assert!(w.due(t0 + CHECK_EVERY));
    }

    #[test]
    fn the_notice_names_the_game_plainly_and_always_shows() {
        let v: serde_json::Value = serde_json::from_str(&notice_json(&rl().unwrap())).unwrap();
        assert_eq!(v["type"], "notice");
        assert_eq!(v["always"], true);
        assert_eq!(v["kind"], "refused");
        assert!(v["detail"].as_str().unwrap().starts_with("Rocket League runs in exclusive fullscreen."));
        assert_eq!(short_name("VALORANT"), "VALORANT");
    }
}
