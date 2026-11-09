//! The user's game registrations (Settings -> Games: `[[overrides]]` and
//! `detect.fullscreen_games`, Rohan 2026-11-07 "support for manual application
//! registering") changing while a session runs. A new registration applies
//! from the next foreground observation; this module ends a live session that
//! the change took away:
//!
//! - "Never record" (`kind = "none"`) for the recorded exe ends its session;
//! - removing the entry that made it a target ends the session unless the
//!   proof rule still records it with no check (Steam, a launcher folder or an
//!   Xbox / Store package unless the private list holds it back, a known
//!   game). Anything else is read again at once: a game verdict records it
//!   again, no verdict leaves it unrecorded with the G4 notice (R8-detect-5);
//! - either way the engine stops capturing it now and its unsaved ring is
//!   dropped (no drain linger, R8-detect-2), also when its window cannot be
//!   probed.
//!
//! The tray's "Add <exe> as a game" (offered after the G4 notice names an
//! exe) registers it through the same per-app op Settings sends
//! (`record_game_ops`); it never turns Never record back into Record, and it
//! goes once Settings registers the exe (R8-detect-8).

use std::time::Instant;

use crate::config::Config;
use crate::log;
use crate::record_app::Controller;
use crate::record_game_ops::{offer_retired, tray_add_patch};
use crate::record_target::{classify_event, is_fullscreen_game, skips_render_check, Target};
use crate::record_transition::Transitions;
use crate::rules::override_kind;
use crate::types::RuleKind;
use crate::watcher::{self, WatchEvent};

/// `exe` (a stem or a file name) is registered by name under `cfg`: a game
/// override or a `fullscreen_games` entry, whatever its window is now.
/// Recall and Windows never are (a hand-edited entry, gate p5-3: `never_recorded`).
fn registered_name(cfg: &Config, exe: &str) -> bool {
    !crate::never_recorded::never_recordable(exe) && (matches!(override_kind(cfg, exe), Some(RuleKind::HdrGame | RuleKind::SdrGame)) || is_fullscreen_game(cfg, exe))
}

/// `ev` is a target under `cfg` by its registration: a game override, or a
/// `fullscreen_games` entry for a fullscreen window.
fn registered(cfg: &Config, ev: &WatchEvent) -> bool {
    !crate::never_recorded::never_recordable(&ev.exe_stem)
        && (matches!(override_kind(cfg, &ev.exe_stem), Some(RuleKind::HdrGame | RuleKind::SdrGame)) || (ev.fullscreen && is_fullscreen_game(cfg, &ev.exe_stem)))
}

/// The session on `ev` (the recorded window, as it is now) ends because the
/// registrations went from `old` to `new`. It stays only when the proof rule
/// still records it with no check (R8-detect-5): the private list, which beats
/// a launcher folder or a Store package, is applied by `classify_event`.
pub(crate) fn ends_session(old: &Config, new: &Config, ev: &WatchEvent) -> bool {
    if override_kind(new, &ev.exe_stem) == Some(RuleKind::None) {
        return true;
    }
    let proven = classify_event(ev, new).is_some() && skips_render_check(ev, new);
    registered_name(old, &ev.exe_stem) && !registered(new, ev) && !proven
}

/// [`ends_session`] for the live session `t`; `probed` = its window as it is now,
/// `None` when it could not be read (then Never record still ends it, and so
/// does taking away the registration that named it, R8-detect-2).
pub(crate) fn session_ends(old: &Config, new: &Config, t: &Target, probed: Option<&WatchEvent>) -> bool {
    if override_kind(new, &t.exe) == Some(RuleKind::None) {
        return true;
    }
    match probed {
        Some(ev) => ends_session(old, new, ev),
        None => registered_name(old, &t.exe) && !registered_name(new, &t.exe),
    }
}

/// The registrations differ between `old` and `new`.
pub(crate) fn registrations_changed(old: &Config, new: &Config) -> bool {
    old.overrides != new.overrides || old.detect.fullscreen_games != new.detect.fullscreen_games
}

impl Controller {
    /// The tray's "Add <exe> as a game": register the offered exe, then take
    /// the item out of the menu. An exe set to Never record (or already a game)
    /// is left as it is (R8-detect-8).
    pub(crate) fn add_offered_game(&mut self) {
        let Some(exe) = self.tray.offered_game().map(str::to_string) else { return };
        let mut added = false;
        if self.apply_config_change(None, |cfg| {
            let patch = tray_add_patch(cfg, &exe);
            added = patch.is_some();
            patch
        }) {
            if added {
                log!("record: tray: {exe} added as a game");
                self.shortcut_notice(&format!("Added {exe} as a game"), "Recall records it from now on");
            }
            self.tray.offer_game(None);
        }
    }

    /// After a config change: end the live session if its registration was
    /// taken away ([`session_ends`]) and drop its ring; the foreground is then
    /// read again at once under the current rules (R8-detect-5). A tray offer
    /// for an exe Settings just registered goes (R8-detect-8).
    pub(crate) fn recheck_registration(&mut self, old: &Config) {
        if !registrations_changed(old, &self.cfg) {
            return;
        }
        if self.tray.offered_game().is_some_and(|exe| offer_retired(&self.cfg, exe)) {
            self.tray.offer_game(None);
        }
        // A window fed before the change is fed again on its next observation.
        self.last_fed = None;
        let probed = self.tracker.current().and_then(|t| watcher::probe_window(t.hwnd, self.target_monitor).filter(|e| e.pid == t.pid));
        if self.end_for_registration(old, probed.as_ref()) {
            self.probe_foreground(Instant::now(), false);
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

    /// Gate p5-3: a hand-edited Windows entry ("explorer", any case, a path) is no registration:
    /// it never records, so it neither counts as registered nor retires anything.
    #[test]
    fn a_hand_edited_windows_entry_is_not_a_registration() {
        for exe in ["explorer", "EXPLORER.EXE", r"C:\Windows\explorer.exe", "Mystify.scr"] {
            let cfg = with(exe, RuleKind::SdrGame);
            assert!(!registered_name(&cfg, exe), "{exe}");
            assert!(!registered_name(&cfg, "explorer"), "{exe} vs the stem");
        }
    }

    #[test]
    fn the_tray_add_writes_a_game_override_the_controller_accepts() {
        let cfg = with("Other", RuleKind::HdrGame);
        let patch = tray_add_patch(&cfg, "MysteryApp.exe").expect("not registered yet");
        let out = crate::record_config::apply_patch(&cfg, &patch).expect("valid patch").config;
        let kinds: Vec<(&str, RuleKind)> = out.overrides.iter().map(|o| (o.exe.as_str(), o.kind)).collect();
        assert!(kinds.contains(&("MysteryApp.exe", RuleKind::SdrGame)), "{kinds:?}");
        assert!(kinds.contains(&("Other", RuleKind::HdrGame)), "others kept");
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

    #[test]
    fn the_private_list_beats_a_launcher_folder_when_the_override_goes_r8_detect_5() {
        let launcher = WatchEvent { launcher_game: true, ..ev("Element", true) };
        assert!(ends_session(&with("Element", RuleKind::SdrGame), &Config::default(), &launcher), "a chat app in a GOG folder");
        let store = WatchEvent { store_game: true, ..ev("Element", true) };
        assert!(ends_session(&with("Element", RuleKind::SdrGame), &Config::default(), &store));
        let game = WatchEvent { launcher_game: true, ..ev("SomeGame", true) };
        assert!(!ends_session(&with("SomeGame", RuleKind::SdrGame), &Config::default(), &game), "a launcher game stays");
    }

    #[test]
    fn removing_discord_ends_its_session_even_windowed_r8_detect_1() {
        let listed = Config::default();
        let mut unlisted = listed.clone();
        unlisted.detect.fullscreen_games.clear();
        assert!(ends_session(&listed, &unlisted, &ev("Discord", false)));
    }

    #[test]
    fn an_unreadable_window_still_ends_for_never_record_or_its_removal_r8_detect_2() {
        let t = Target { hwnd: 0x50, pid: 5, name: "MysteryApp".into(), exe: "MysteryApp.exe".into() };
        let added = with("MysteryApp", RuleKind::SdrGame);
        assert!(session_ends(&added, &with("mysteryapp", RuleKind::None), &t, None), "Never record");
        assert!(session_ends(&added, &Config::default(), &t, None), "removed");
        let mut both = added.clone();
        both.overrides.push(Override { exe: "Other".into(), kind: RuleKind::SdrGame, clamp: None });
        assert!(!session_ends(&added, &both, &t, None), "unrelated change");
    }
}
