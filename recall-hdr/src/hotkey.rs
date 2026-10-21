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
        let hotkey = parse_accel(accel)?;

        let manager = GlobalHotKeyManager::new()
            .map_err(|e| format!("global-hotkey manager init failed: {e}"))?;

        manager
            .register(hotkey)
            .map_err(|e| format!("register hotkey '{accel}' failed: {e}"))?;

        let (save, warn) = match save_accel {
            None => (None, None),
            Some(s) => match parse_accel(s).and_then(|hk| {
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

/// What the record mode's accelerators reported since the last poll.
#[derive(Default, Clone, Copy, Debug)]
pub struct RecordFired {
    pub save: bool,
    pub mic: bool,
    pub hdr: bool,
}

/// The unified record mode's accelerators (save replay, toggle mic, and the HDR
/// toggle only while HDR management is on), re-bindable at runtime on ONE
/// manager. Same pump-thread / single-instance contract as [`Hotkey`] -- the
/// two are never alive at once (they belong to different modes).
pub struct RecordHotkeys {
    manager: GlobalHotKeyManager,
    save: Option<HotKey>,
    mic: Option<HotKey>,
    hdr: Option<HotKey>,
}

impl RecordHotkeys {
    pub fn new() -> Result<Self, String> {
        let manager = GlobalHotKeyManager::new().map_err(|e| format!("global-hotkey manager init failed: {e}"))?;
        Ok(RecordHotkeys { manager, save: None, mic: None, hdr: None })
    }

    /// Replace every binding. Returns one warning per accelerator that could not
    /// be parsed/registered (the others still work).
    pub fn bind(&mut self, save: &str, mic: &str, hdr: Option<&str>) -> Vec<String> {
        for hk in [self.save.take(), self.mic.take(), self.hdr.take()].into_iter().flatten() {
            let _ = self.manager.unregister(hk);
        }
        let mut warnings = Vec::new();
        let mut reg = |accel: &str| match parse_accel(accel).and_then(|hk| {
            self.manager.register(hk).map(|_| hk).map_err(|e| format!("register hotkey '{accel}' failed: {e}"))
        }) {
            Ok(hk) => Some(hk),
            Err(e) => {
                warnings.push(e);
                None
            }
        };
        let save_hk = reg(save);
        let mic_hk = reg(mic);
        let hdr_hk = hdr.and_then(&mut reg);
        self.save = save_hk;
        self.mic = mic_hk;
        self.hdr = hdr_hk;
        warnings
    }

    /// Drain the process-global hotkey channel (non-blocking, presses only).
    pub fn poll(&self) -> RecordFired {
        let mut fired = RecordFired::default();
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            let is = |hk: Option<HotKey>| hk.is_some_and(|h| h.id() == event.id);
            fired.save |= is(self.save);
            fired.mic |= is(self.mic);
            fired.hdr |= is(self.hdr);
        }
        fired
    }
}

impl Drop for RecordHotkeys {
    fn drop(&mut self) {
        for hk in [self.save, self.mic, self.hdr].into_iter().flatten() {
            let _ = self.manager.unregister(hk);
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
}
