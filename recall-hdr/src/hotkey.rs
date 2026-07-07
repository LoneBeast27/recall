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

/// A registered global toggle hotkey. Holds the manager alive (dropping it
/// unregisters) and remembers the parsed `HotKey` so [`poll`](Hotkey::poll) can
/// filter the shared event stream down to our own id.
pub struct Hotkey {
    manager: GlobalHotKeyManager,
    hotkey: HotKey,
}

impl Hotkey {
    /// Parse `accel` (e.g. `"Ctrl+Alt+H"`, `"Win+Shift+F1"`) and register it as a
    /// system-wide hotkey.
    ///
    /// Returns `Err(String)` (never panics) on a malformed accelerator, on manager
    /// creation failure, or if the OS refuses the registration (e.g. the combo is
    /// already owned by another app). Must be called on the message-pump thread.
    pub fn register(accel: &str) -> Result<Hotkey, String> {
        let hotkey = parse_accel(accel)?;

        let manager = GlobalHotKeyManager::new()
            .map_err(|e| format!("global-hotkey manager init failed: {e}"))?;

        manager
            .register(hotkey)
            .map_err(|e| format!("register hotkey '{accel}' failed: {e}"))?;

        Ok(Hotkey { manager, hotkey })
    }

    /// Drain the process-global hotkey event channel and report whether our toggle
    /// fired since the last poll.
    ///
    /// Non-blocking: uses `try_recv` until the channel is empty. Only
    /// [`HotKeyState::Pressed`] events matching our registered id count; the paired
    /// `Released` event and any (currently none) foreign ids are discarded. If the
    /// key was pressed multiple times between polls it still reports a single
    /// `true` -- the toggle is level-triggered, not a counter.
    pub fn poll(&self) -> bool {
        let mut fired = false;
        let rx = GlobalHotKeyEvent::receiver();
        while let Ok(event) = rx.try_recv() {
            if event.id == self.hotkey.id() && event.state == HotKeyState::Pressed {
                fired = true;
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
    }
}

/// Parse an accelerator string into a [`HotKey`], normalizing `Win` -> `Super`.
///
/// Split out from [`Hotkey::register`] so it is unit-testable without touching the
/// OS (the `GlobalHotKeyManager` needs a real message loop / thread affinity).
fn parse_accel(accel: &str) -> Result<HotKey, String> {
    let normalized = normalize_win(accel);
    HotKey::from_str(&normalized).map_err(|e| format!("invalid accelerator '{accel}': {e}"))
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
}
