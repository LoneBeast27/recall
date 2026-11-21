//! Global hotkey (global-hotkey 0.8): parse the config accelerator string and
//! deliver toggle-HDR presses to the main message loop.
//!
//! # Contract with the app layer
//! - [`Hotkey::register`] MUST be called on the thread that runs the Win32 message
//!   pump. `global-hotkey` on Windows uses a low-level keyboard hook / `RegisterHotKey`
//!   bound to the creating thread's message queue, so the manager has to live on the
//!   pump thread and events are only pumped while that loop runs.
//! - Events arrive through the crate's **process-global** receiver
//!   ([`GlobalHotKeyEvent::receiver`]), not a per-instance channel. [`Hotkey::poll`]
//!   drains it each loop iteration and reports whether *our* toggle fired.
//! - The hotkey is unregistered on [`Drop`], so dropping the `Hotkey` (or letting the
//!   app tear down) releases the OS registration.
//!
//! # 0.8 API notes (differs from the 0.6-era design assumptions)
//! - `HotKey` implements `FromStr` (via `parse_hotkey`). Recognized modifier tokens
//!   are `Ctrl`/`Control`, `Alt`/`Option`, `Shift`, and `Cmd`/`Command`/`Super`
//!   (plus `CmdOrCtrl`). **There is no `Win` token** -- the raw parser would treat
//!   `Win` as a key and fail. We normalize `Win` -> `Super` before parsing so the
//!   documented config form `"Win+Shift+F1"` works and maps to the Windows key.
//! - `GlobalHotKeyEvent` carries `{ id: u32, state: HotKeyState }`. We count only
//!   `HotKeyState::Pressed` (ignore the paired `Released`) and only for our id.

use std::str::FromStr;

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::record_hotkeys::{split_conflicts, Binding, HotkeyAction};

/// A registered global toggle hotkey. Holds the manager alive (dropping it
/// unregisters) and remembers the parsed `HotKey` so [`poll`](Hotkey::poll) can
/// filter the shared event stream down to our own id.
pub struct Hotkey {
    manager: GlobalHotKeyManager,
    hotkey: HotKey,
    /// Optional second accelerator: save the engine's replay buffer.
    save: Option<HotKey>,
}

/// What fired since the last poll (drained from ONE shared channel — never
/// instantiate two `Hotkey`s, they would steal each other's events).
#[derive(Default, Clone, Copy)]
pub struct Fired {
    pub toggle_hdr: bool,
    pub save_clip: bool,
}

impl Hotkey {
    /// Parse + register the toggle accelerator (hard error) and, optionally, a
    /// save-clip accelerator (best-effort: failure is returned as a warning so
    /// the toggle still works). Must be called on the message-pump thread.
    pub fn register(accel: &str, save_accel: Option<&str>) -> Result<(Hotkey, Option<String>), String> {
        let hotkey = parse_bindable(accel)?;

        let manager = GlobalHotKeyManager::new()
            .map_err(|e| format!("global-hotkey manager init failed: {e}"))?;

        manager
            .register(hotkey)
            .map_err(|e| format!("register hotkey '{accel}' failed: {e}"))?;

        let (save, warn) = match save_accel {
            None => (None, None),
            Some(s) => match parse_bindable(s).and_then(|hk| {
                manager
                    .register(hk)
                    .map(|_| hk)
                    .map_err(|e| format!("register hotkey '{s}' failed: {e}"))
            }) {
                Ok(hk) => (Some(hk), None),
                Err(e) => (None, Some(e)),
            },
        };

        Ok((Hotkey { manager, hotkey, save }, warn))
    }

    /// Drain the process-global hotkey event channel and report which of our
    /// accelerators fired since the last poll.
    ///
    /// Non-blocking: uses `try_recv` until the channel is empty. Only
    /// [`HotKeyState::Pressed`] events matching our registered ids count; the
    /// paired `Released` events and foreign ids are discarded. Level-triggered:
    /// multiple presses between polls still report a single `true`.
    pub fn poll(&self) -> Fired {
        let mut fired = Fired::default();
        let rx = GlobalHotKeyEvent::receiver();
        while let Ok(event) = rx.try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            if event.id == self.hotkey.id() {
                fired.toggle_hdr = true;
            }
            if self.save.is_some_and(|hk| hk.id() == event.id) {
                fired.save_clip = true;
            }
        }
        fired
    }

    /// The parsed hotkey (introspection / logging).
    pub fn hotkey(&self) -> HotKey {
        self.hotkey
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        // Best-effort: a failed unregister on teardown is not actionable and must
        // not panic in a Drop.
        let _ = self.manager.unregister(self.hotkey);
        if let Some(save) = self.save {
            let _ = self.manager.unregister(save);
        }
    }
}

/// The unified record mode's accelerators (save presets, mic, bookmark,
/// screenshot, long recording, and the HDR toggle only while HDR management
/// is on), re-bindable at runtime on ONE manager. Same pump-thread /
/// single-instance contract as [`Hotkey`] -- the two are never alive at once
/// (they belong to different modes).
pub struct RecordHotkeys {
    manager: GlobalHotKeyManager,
    bound: Vec<(HotKey, HotkeyAction)>,
    /// Invalid or duplicate bindings from the last bind (not retried).
    conflicts: Vec<String>,
    /// Bindings the OS refused, retried every 10 s.
    refused: crate::record_hotkey_retry::Refused,
}

impl RecordHotkeys {
    pub fn new() -> Result<Self, String> {
        let manager = GlobalHotKeyManager::new().map_err(|e| format!("global-hotkey manager init failed: {e}"))?;
        Ok(RecordHotkeys { manager, bound: Vec::new(), conflicts: Vec::new(), refused: Default::default() })
    }

    /// Replace every binding with `list`. Returns one warning per accelerator
    /// that is invalid, collides with an earlier one, or the OS refused (held
    /// by another app); every other binding still works.
    pub fn bind(&mut self, list: Vec<Binding>) -> Vec<String> {
        for (hk, _) in self.bound.drain(..) {
            let _ = self.manager.unregister(hk);
        }
        let (keep, conflicts) = split_conflicts(list);
        let mut refused = Vec::new();
        for (b, hk) in keep {
            match self.manager.register(hk) {
                Ok(()) => self.bound.push((hk, b.action)),
                Err(e) => refused.push((b, hk, e.to_string())),
            }
        }
        self.conflicts = conflicts;
        self.refused.set(refused, std::time::Instant::now());
        self.problems()
    }

    /// Conflicts, then the accelerators the OS refused.
    pub fn problems(&self) -> Vec<String> {
        self.conflicts.iter().cloned().chain(self.refused.warnings()).collect()
    }

    /// Every 10 s while the OS refused some accelerator: try those again. `None` when no retry
    /// was due; else the labels that registered now.
    pub fn retry_refused(&mut self, now: std::time::Instant) -> Option<Vec<String>> {
        if !self.refused.due(now) {
            return None;
        }
        let manager = &self.manager;
        let won = self.refused.retry(now, |hk| manager.register(hk).map_err(|e| e.to_string()));
        let labels = won.iter().map(|(b, _)| b.label.clone()).collect();
        self.bound.extend(won.into_iter().map(|(b, hk)| (hk, b.action)));
        Some(labels)
    }

    /// The actions now registered (for the log).
    pub fn bound(&self) -> Vec<HotkeyAction> {
        self.bound.iter().map(|(_, a)| *a).collect()
    }

    /// Drain the process-global hotkey channel (non-blocking, presses only).
    /// Each action is reported once however often it fired; unknown ids are
    /// dropped.
    pub fn poll(&self) -> Vec<HotkeyAction> {
        let mut fired = Vec::new();
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            if let Some(action) = action_for(&self.bound, event.id) {
                if !fired.contains(&action) {
                    fired.push(action);
                }
            }
        }
        fired
    }
}

/// The action bound to accelerator `id`, if any.
fn action_for(bound: &[(HotKey, HotkeyAction)], id: u32) -> Option<HotkeyAction> {
    bound.iter().find(|(hk, _)| hk.id() == id).map(|(_, a)| *a)
}

impl Drop for RecordHotkeys {
    fn drop(&mut self) {
        for (hk, _) in &self.bound {
            let _ = self.manager.unregister(*hk);
        }
    }
}

/// Parse an accelerator string into a [`HotKey`], normalizing `Win` -> `Super`.
///
/// Split out from [`Hotkey::register`] so it is unit-testable without touching the
/// OS (the `GlobalHotKeyManager` needs a real message loop / thread affinity).
pub(crate) fn parse_accel(accel: &str) -> Result<HotKey, String> {
    let normalized = normalize_win(accel);
    HotKey::from_str(&normalized).map_err(|e| format!("invalid accelerator '{accel}': {e}"))
}

/// Combos Windows itself acts on in every window. `RegisterHotKey` accepts most of them and
/// then takes them from every app (an Alt+F4 shortcut would stop Alt+F4 closing a game), so
/// Recall never REGISTERS one. A stored one is kept as it is (old settings load unchanged:
/// the v0.1.x compatibility fixtures hold Alt+F4); the app offers to change it, and its
/// picker refuses them for new picks. Win+ combos Explorer holds are refused by the OS.
pub(crate) const WINDOWS_OWNED: [(&str, &str); 8] = [
    ("Alt+F4", "it closes the window in front"),
    ("Alt+Tab", "it switches windows"),
    ("Alt+Shift+Tab", "it switches windows"),
    ("Alt+Escape", "it switches windows"),
    ("Alt+Space", "it opens the window menu"),
    ("Ctrl+Escape", "it opens Start"),
    ("Ctrl+Shift+Escape", "it opens Task Manager"),
    ("Ctrl+Alt+Delete", "it opens the security screen"),
];

/// What Windows does with `hk`, when it is one of [`WINDOWS_OWNED`].
pub(crate) fn windows_owned(hk: &HotKey) -> Option<&'static str> {
    WINDOWS_OWNED
        .iter()
        .find(|(combo, _)| parse_accel(combo).is_ok_and(|c| c.id() == hk.id()))
        .map(|(_, what)| *what)
}

/// [`parse_accel`] for registering: a [`WINDOWS_OWNED`] combo is an error.
pub(crate) fn parse_bindable(accel: &str) -> Result<HotKey, String> {
    let hk = parse_accel(accel)?;
    match windows_owned(&hk) {
        Some(what) => Err(format!("'{accel}' belongs to Windows ({what}), so Recall does not use it")),
        None => Ok(hk),
    }
}

/// Rewrite any `Win` modifier token to `Super` (case-insensitively), preserving the
/// rest of the string. global-hotkey 0.8 recognizes `Super`/`Cmd`/`Command` for the
/// meta key but NOT `Win`; the config/UI uses the friendlier `Win`.
///
/// Only whole `+`-delimited tokens equal to `win` are rewritten, so a key like a
/// hypothetical `Windows`-named code is left alone (there is none today, but this
/// keeps the rewrite conservative).
fn normalize_win(accel: &str) -> String {
    accel
        .split('+')
        .map(|tok| {
            if tok.trim().eq_ignore_ascii_case("win") {
                // Preserve surrounding whitespace shape minimally: emit the trimmed
                // replacement (the parser trims tokens anyway).
                "Super"
            } else {
                tok
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use global_hotkey::hotkey::{Code, Modifiers};

    #[test]
    fn parses_ctrl_alt_h() {
        let hk = parse_accel("Ctrl+Alt+H").expect("Ctrl+Alt+H should parse");
        assert_eq!(hk.key, Code::KeyH);
        assert!(hk.mods.contains(Modifiers::CONTROL));
        assert!(hk.mods.contains(Modifiers::ALT));
        assert!(!hk.mods.contains(Modifiers::SHIFT));
    }

    #[test]
    fn parses_win_shift_f1_via_super_normalization() {
        // "Win" is NOT a global-hotkey token; our normalize_win rewrites it to
        // "Super" so this documented config form works and maps to the meta key.
        let hk = parse_accel("Win+Shift+F1").expect("Win+Shift+F1 should parse");
        assert_eq!(hk.key, Code::F1);
        assert!(hk.mods.contains(Modifiers::SUPER), "Win must map to SUPER");
        assert!(hk.mods.contains(Modifiers::SHIFT));
    }

    #[test]
    fn win_is_case_insensitive() {
        let hk = parse_accel("WIN+A").expect("WIN+A should parse");
        assert!(hk.mods.contains(Modifiers::SUPER));
        assert_eq!(hk.key, Code::KeyA);
    }

    #[test]
    fn super_and_cmd_still_work_directly() {
        // Normalization must not disturb the native meta tokens.
        for s in ["Super+A", "Cmd+A"] {
            let hk = parse_accel(s).unwrap_or_else(|e| panic!("{s} should parse: {e}"));
            assert!(hk.mods.contains(Modifiers::SUPER), "{s} -> SUPER");
        }
    }

    #[test]
    fn rejects_modifiers_only() {
        // No main key -> Err, not a panic.
        assert!(parse_accel("Ctrl+Shift").is_err(), "modifiers-only must error");
    }

    #[test]
    fn rejects_empty_string() {
        assert!(parse_accel("").is_err(), "empty accelerator must error");
    }

    #[test]
    fn rejects_garbage_key() {
        assert!(
            parse_accel("Ctrl+Nonsense").is_err(),
            "unknown key token must error"
        );
    }

    #[test]
    fn default_config_hotkey_parses() {
        // The shipped default accelerator (config::HotkeyCfg::default) must be valid.
        assert!(parse_accel("Ctrl+Alt+H").is_ok());
    }

    #[test]
    fn record_mode_default_hotkeys_parse() {
        let save = parse_accel("Alt+N").expect("Alt+N");
        let mic = parse_accel("Alt+M").expect("Alt+M");
        assert_eq!(save.key, Code::KeyN);
        assert_eq!(mic.key, Code::KeyM);
        assert_ne!(save.id(), mic.id(), "distinct ids so poll can tell them apart");
    }

    #[test]
    fn function_keys_parse_alone_and_with_modifiers() {
        for (accel, key) in [("F10", Code::F10), ("Alt+F10", Code::F10), ("Ctrl+F9", Code::F9), ("Shift+F1", Code::F1), ("F24", Code::F24)] {
            let hk = parse_accel(accel).unwrap_or_else(|e| panic!("{accel}: {e}"));
            assert_eq!(hk.key, key, "{accel}");
        }
        assert!(parse_accel("Alt+F10").unwrap().mods.contains(Modifiers::ALT));
        assert!(parse_accel("F10").unwrap().mods.is_empty(), "a bare F-key has no modifier");
        assert_ne!(parse_accel("F10").unwrap().id(), parse_accel("Alt+F10").unwrap().id());
    }

    #[test]
    fn combos_windows_owns_are_never_bindable_in_any_spelling() {
        for accel in ["Alt+F4", "alt+f4", "Alt+Tab", "Shift+Alt+Tab", "Alt+Escape", "Alt+Space", "Ctrl+Escape", "Ctrl+Shift+Escape", "Ctrl+Alt+Delete"] {
            let err = parse_bindable(accel).expect_err(accel);
            assert!(err.contains("belongs to Windows"), "{accel}: {err}");
            assert!(parse_accel(accel).is_ok(), "{accel}: still parses, so stored settings load unchanged");
        }
        assert!(parse_bindable("Alt+F4").unwrap_err().contains("closes the window in front"));
        // Neighbours stay free: only the exact combo is Windows'.
        for accel in ["F4", "Ctrl+F4", "Alt+Shift+F4", "Alt+F3", "Ctrl+Tab", "Alt+F10", "F10"] {
            assert!(parse_bindable(accel).is_ok(), "{accel}");
        }
    }

    #[test]
    fn poll_maps_each_accelerator_id_to_its_action() {
        let bound = vec![
            (parse_accel("Alt+N").unwrap(), HotkeyAction::Save { seconds: 0 }),
            (parse_accel("Alt+B").unwrap(), HotkeyAction::Save { seconds: 30 }),
            (parse_accel("Alt+R").unwrap(), HotkeyAction::Record),
        ];
        assert_eq!(action_for(&bound, parse_accel("Alt+B").unwrap().id()), Some(HotkeyAction::Save { seconds: 30 }));
        assert_eq!(action_for(&bound, parse_accel("Alt+R").unwrap().id()), Some(HotkeyAction::Record));
        assert_eq!(action_for(&bound, parse_accel("Alt+Q").unwrap().id()), None);
    }
}
