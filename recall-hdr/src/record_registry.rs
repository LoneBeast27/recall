//! The user's game registrations (Settings -> Games: `[[overrides]]` and
//! `detect.fullscreen_games`, Rohan 2026-11-07 "support for manual application
//! registering") changing while a session runs. A new registration applies
//! from the next foreground observation; this module ends a live session that
//! the change took away:
//!
//! - "Never record" (`kind = "none"`) for the recorded exe ends its session;
//! - removing the entry that made it a target ends the session unless an
//!   official install proves it anyway (Steam, a launcher folder, an Xbox /
//!   Store package, a known game). Anything else is detected again on the
//!   next observation: a game verdict records it again, no verdict leaves it
//!   unrecorded with the G4 notice.
//!
//! The tray's "Add <exe> as a game" (offered after the G4 notice names an
//! exe) registers it with the same `set_config` patch Settings sends.

use serde_json::{json, Value};

use crate::config::{Config, Override};
use crate::log;
use crate::record_app::Controller;
use crate::record_target::{game_evidence, is_fullscreen_game};
use crate::record_target_rules::official_install;
use crate::rules::override_kind;
use crate::types::RuleKind;
use crate::watcher::{self, WatchEvent};

/// `stem` is registered as a game under `cfg`: a game override, or a
/// `fullscreen_games` entry for a fullscreen window.
fn registered(cfg: &Config, ev: &WatchEvent) -> bool {
    matches!(override_kind(cfg, &ev.exe_stem), Some(RuleKind::HdrGame | RuleKind::SdrGame)) || (ev.fullscreen && is_fullscreen_game(cfg, &ev.exe_stem))
}

/// The session on `ev` (the recorded window, as it is now) ends because the
/// registrations went from `old` to `new`.
pub(crate) fn ends_session(old: &Config, new: &Config, ev: &WatchEvent) -> bool {
    if override_kind(new, &ev.exe_stem) == Some(RuleKind::None) {
        return true;
    }
    let proven = official_install(ev) || game_evidence(ev);
    registered(old, ev) && !registered(new, ev) && !proven
}

/// The registrations differ between `old` and `new`.
pub(crate) fn registrations_changed(old: &Config, new: &Config) -> bool {
    old.overrides != new.overrides || old.detect.fullscreen_games != new.detect.fullscreen_games
}

/// The `set_config` patch that registers `exe` as a game: an override of the
/// config's default game class (or `sdr_game`), replacing any override of the
/// same exe (G9: `Game.exe` = `game`) and keeping every other one.
pub(crate) fn add_game_patch(cfg: &Config, exe: &str) -> Value {
    let kind = match cfg.behavior.default_game_class {
        RuleKind::HdrGame => RuleKind::HdrGame,
        _ => RuleKind::SdrGame,
    };
    let mut list: Vec<Override> = cfg.overrides.iter().filter(|o| !crate::rules::same_exe(&o.exe, exe)).cloned().collect();
    list.push(Override { exe: exe.trim().to_string(), kind, clamp: None });
    json!({ "overrides": list })
}

impl Controller {
    /// The tray's "Add <exe> as a game": register the offered exe, then take
    /// the item out of the menu.
    pub(crate) fn add_offered_game(&mut self) {
        let Some(exe) = self.tray.offered_game().map(str::to_string) else { return };
        if self.apply_config_patch(&add_game_patch(&self.file_cfg, &exe), None) {
            log!("record: tray: {exe} added as a game");
            self.shortcut_notice(&format!("Added {exe} as a game"), "Recall records it from now on");
            self.tray.offer_game(None);
        }
    }

    /// After a config change: end the live session if its registration was
    /// taken away ([`ends_session`]).
    pub(crate) fn recheck_registration(&mut self, old: &Config) {
        if !registrations_changed(old, &self.cfg) {
            return;
        }
        let Some(t) = self.tracker.current().cloned() else { return };
        let Some(ev) = watcher::probe_window(t.hwnd, self.target_monitor).filter(|e| e.pid == t.pid) else { return };
        if ends_session(old, &self.cfg, &ev) {
            log!("record: {} is no longer registered as a game in Settings; its session ends", t.exe);
            self.tracker.on_process_exit(t.pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Override;

    fn ev(stem: &str, fullscreen: bool) -> WatchEvent {
        WatchEvent { exe_stem: stem.into(), exe_name: format!("{stem}.exe"), fullscreen, fullscreen_on_target: fullscreen, ..WatchEvent::unknown(5, 0x50) }
    }

    fn with(exe: &str, kind: RuleKind) -> Config {
        let mut cfg = Config::default();
        cfg.overrides.push(Override { exe: exe.into(), kind, clamp: None });
        cfg
    }

    #[test]
    fn the_tray_add_writes_a_game_override_the_controller_accepts() {
        let mut cfg = with("mysteryapp", RuleKind::None);
        cfg.overrides.push(Override { exe: "Other".into(), kind: RuleKind::HdrGame, clamp: None });
        let patch = add_game_patch(&cfg, "MysteryApp.exe");
        let out = crate::record_config::apply_patch(&cfg, &patch).expect("valid patch").config;
        let kinds: Vec<(&str, RuleKind)> = out.overrides.iter().map(|o| (o.exe.as_str(), o.kind)).collect();
        assert!(kinds.contains(&("MysteryApp.exe", RuleKind::SdrGame)), "{kinds:?}");
        assert!(kinds.contains(&("Other", RuleKind::HdrGame)), "others kept");
        assert!(!kinds.iter().any(|(e, _)| *e == "mysteryapp"), "the old entry for the same exe is replaced: {kinds:?}");
        assert!(crate::record_target::classify_event(&ev("MysteryApp", true), &out).is_some());
    }

    #[test]
    fn removing_or_blocking_a_registration_ends_its_session() {
        let base = Config::default();
        let added = with("MysteryApp.exe", RuleKind::SdrGame);
        assert!(registrations_changed(&added, &base));
        assert!(ends_session(&added, &base, &ev("MysteryApp", true)), "removed: no proof left");
        assert!(ends_session(&base, &with("mysteryapp", RuleKind::None), &ev("MysteryApp", true)), "Never record");
        assert!(!ends_session(&base, &added, &ev("MysteryApp", true)), "added: it stays");
        let steam = WatchEvent { steam_game: true, ..ev("MysteryApp", true) };
        assert!(!ends_session(&added, &base, &steam), "Steam still proves it");
        assert!(ends_session(&base, &with("MysteryApp", RuleKind::None), &steam), "Never record beats Steam");
        let mut listed = Config::default();
        listed.detect.fullscreen_games = vec!["Chatty".into()];
        let mut unlisted = listed.clone();
        unlisted.detect.fullscreen_games.clear();
        assert!(ends_session(&listed, &unlisted, &ev("Chatty", true)), "fullscreen_games entry removed");
        assert!(!ends_session(&base, &base, &ev("Other", true)), "unrelated");
        assert!(!registrations_changed(&base, &base.clone()));
    }
}
