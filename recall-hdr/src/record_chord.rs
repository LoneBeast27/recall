//! The advanced-shortcut chord (user rulings 2026-10-22, 03:36 and 03:38 IST).
//!
//! Leader `[hotkeys] leader` (default `Alt+Backquote` = Alt + the PHYSICAL key
//! left of `1`), then within `window_ms` (1.5 s) one of:
//! `1` Bookmark · `2` Save the last `save_seconds` · `3` Screenshot ·
//! `4` Start/stop recording · `5` Performance stats on/off (Recall Island,
//! 2026-10-26) · `Esc` cancel.
//!
//! Why these keys: the left hand stays on WASD -- thumb on Alt, ring finger
//! reaches the key left of 1, the fingers already sit over 1-5. Ctrl / Shift
//! leaders are avoided because their key-down leaks to the game as crouch /
//! sprint. The follow-up keys are registered as temporary global hotkeys only
//! while the window is open (bare, and with the leader's modifiers in case Alt
//! is still held), so the game never receives them; a timeout, an action, Esc
//! or any other new key press closes the window and unregisters them.
//!
//! **Physical leader key.** `Backquote` in the leader means scan code 0x29
//! (left of 1) on the active layout: `MapVirtualKeyW(0x29, MAPVK_VSC_TO_VK)`
//! gives US `VK_OEM_3` (`` ` ``), German `VK_OEM_5` (`^`), French `VK_OEM_7`
//! (`²`)... and the binding uses that key. A layout whose key has no
//! global-hotkey code (UK `VK_OEM_8`) falls back to `VK_OEM_3` with a warning.

use std::time::{Duration, Instant};

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use serde_json::json;

/// Registers / unregisters temporary global hotkeys (the hotkey manager; a
/// recording fake in tests).
pub trait Registrar {
    fn register(&mut self, hk: HotKey) -> Result<(), String>;
    fn unregister(&mut self, hk: HotKey);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChordKey {
    Bookmark,
    SaveShort,
    Screenshot,
    Record,
    /// Toggle the performance-stats island (`overlay.stats`).
    Stats,
    Cancel,
}

/// (code, action, virtual-key) of each follow-up key. Digits are VK_1..VK_5
/// on every layout.
const KEYS: [(Code, ChordKey, u16); 6] = [
    (Code::Digit1, ChordKey::Bookmark, 0x31),
    (Code::Digit2, ChordKey::SaveShort, 0x32),
    (Code::Digit3, ChordKey::Screenshot, 0x33),
    (Code::Digit4, ChordKey::Record, 0x34),
    (Code::Digit5, ChordKey::Stats, 0x35),
    (Code::Escape, ChordKey::Cancel, 0x1B),
];

pub struct Chord {
    window: Duration,
    leader_mods: Modifiers,
    until: Option<Instant>,
    temps: Vec<(HotKey, ChordKey)>,
    /// Keys already down when the window opened (the leader itself).
    held: Vec<u16>,
}

impl Chord {
    pub fn new(window: Duration, leader_mods: Modifiers) -> Self {
        Chord { window, leader_mods, until: None, temps: Vec::new(), held: Vec::new() }
    }

    pub fn is_open(&self) -> bool {
        self.until.is_some()
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.until
    }

    /// New timing / leader modifiers (closes an open window first).
    pub fn configure(&mut self, window: Duration, leader_mods: Modifiers, reg: &mut dyn Registrar) {
        self.close(reg);
        self.window = window;
        self.leader_mods = leader_mods;
    }

    /// Leader pressed: open (or extend) the window and grab the follow-up
    /// keys. `held` = keys down right now. Returns registration warnings.
    pub fn open(&mut self, now: Instant, held: Vec<u16>, reg: &mut dyn Registrar) -> Vec<String> {
        self.until = Some(now + self.window);
        self.held = held;
        if !self.temps.is_empty() {
            return Vec::new();
        }
        let mut warnings = Vec::new();
        for (hk, key) in temp_hotkeys(self.leader_mods) {
            match reg.register(hk) {
                Ok(()) => self.temps.push((hk, key)),
                // Bare keys matter; a taken modifier variant is only logged.
                Err(e) if hk.mods.is_empty() => warnings.push(format!("advanced shortcut key unavailable: {e}")),
                Err(_) => {}
            }
        }
        warnings
    }

    /// A pressed accelerator `id`: the chord key it is (closing the window),
    /// or `None` when the window is shut or the id is not ours.
    pub fn press(&mut self, id: u32, reg: &mut dyn Registrar) -> Option<ChordKey> {
        if !self.is_open() {
            return None;
        }
        let key = self.temps.iter().find(|(hk, _)| hk.id() == id).map(|(_, k)| *k)?;
        self.close(reg);
        Some(key)
    }

    /// Close on timeout or on a new press of any unrelated key (`down` = keys
    /// down now). `true` when it closed here.
    pub fn tick(&mut self, now: Instant, down: &[u16], reg: &mut dyn Registrar) -> bool {
        if !self.is_open() {
            return false;
        }
        let timed_out = self.until.is_some_and(|t| now >= t);
        let stray = down.iter().any(|vk| !self.held.contains(vk) && is_unrelated(*vk));
        if timed_out || stray {
            self.close(reg);
            return true;
        }
        self.held.retain(|vk| down.contains(vk)); // a released key counts again
        false
    }

    pub fn close(&mut self, reg: &mut dyn Registrar) {
        for (hk, _) in self.temps.drain(..) {
            reg.unregister(hk);
        }
        self.until = None;
        self.held.clear();
    }
}

/// The temporary accelerators: each follow-up key bare and, when the leader
/// has modifiers, with them (Esc only bare: Alt+Esc is a Windows shortcut).
pub fn temp_hotkeys(leader_mods: Modifiers) -> Vec<(HotKey, ChordKey)> {
    let mut out = Vec::new();
    for (code, key, _) in KEYS {
        out.push((HotKey::new(None, code), key));
        if !leader_mods.is_empty() && key != ChordKey::Cancel {
            out.push((HotKey::new(Some(leader_mods), code), key));
        }
    }
    out
}

/// Mouse buttons, modifiers and the chord keys never close the window.
fn is_unrelated(vk: u16) -> bool {
    let mouse = matches!(vk, 0x01 | 0x02 | 0x04 | 0x05 | 0x06);
    let modifier = matches!(vk, 0x10..=0x12 | 0xA0..=0xA5 | 0x5B | 0x5C | 0x14);
    let ours = KEYS.iter().any(|(_, _, v)| *v == vk);
    !(mouse || modifier || ours)
}

/// (key, label) of each option, in key order. `recording` = a long recording
/// is running (4 then stops it).
pub fn options(save_seconds: u32, recording: bool) -> Vec<(&'static str, String)> {
    vec![
        ("1", "Bookmark".into()),
        ("2", format!("Save {save_seconds} s")),
        ("3", "Screenshot".into()),
        ("4", if recording { "Stop recording" } else { "Record" }.into()),
        ("5", "Performance stats".into()),
    ]
}

/// `session` `chord` event: active with its options, or inactive.
pub fn event_json(active: bool, options: &[(&str, String)], leader: &str) -> String {
    if !active {
        return json!({"type": "chord", "active": false}).to_string();
    }
    let opts: Vec<_> = options.iter().map(|(k, l)| json!({"key": k, "label": l})).collect();
    json!({"type": "chord", "active": true, "leader": leader, "options": opts}).to_string()
}

/// global-hotkey code for a layout's virtual key (the OEM keys it supports).
pub fn code_for_vk(vk: u16) -> Option<Code> {
    Some(match vk {
        0xC0 => Code::Backquote,
        0xBA => Code::Semicolon,
        0xBB => Code::Equal,
        0xBC => Code::Comma,
        0xBD => Code::Minus,
        0xBE => Code::Period,
        0xBF => Code::Slash,
        0xDB => Code::BracketLeft,
        0xDC => Code::Backslash,
        0xDD => Code::BracketRight,
        0xDE => Code::Quote,
        _ => return None,
    })
}

/// Inverse of [`code_for_vk`] (the OEM keys, whose character depends on the layout).
pub fn vk_for_code(code: Code) -> Option<u16> {
    (0xBAu16..=0xDE).find(|vk| code_for_vk(*vk) == Some(code))
}

/// A `Backquote` accelerator re-aimed at the physical key left of 1 on the
/// layout whose VK for scan code 0x29 is `layout_vk`. Returns the hotkey and
/// a warning when the layout's key cannot be registered.
pub fn resolve_physical(hk: HotKey, layout_vk: Option<u16>) -> (HotKey, Option<String>) {
    if hk.key != Code::Backquote {
        return (hk, None);
    }
    let mods = (!hk.mods.is_empty()).then_some(hk.mods);
    match layout_vk {
        None => (hk, None),
        Some(vk) => match code_for_vk(vk) {
            Some(code) => (HotKey::new(mods, code), None),
            None => (
                hk,
                Some(format!(
                    "the key left of 1 (VK {vk:#04x}) cannot be a global hotkey on this layout; \
                     using the US backquote key instead (set hotkeys.leader to choose another)"
                )),
            ),
        },
    }
}

/// "Alt `": modifiers + the key's character on the active layout.
pub fn leader_label(hk: &HotKey, key_char: Option<char>) -> String {
    let mut parts = Vec::new();
    for (m, name) in [(Modifiers::CONTROL, "Ctrl"), (Modifiers::ALT, "Alt"), (Modifiers::SHIFT, "Shift"), (Modifiers::SUPER, "Win")] {
        if hk.mods.contains(m) {
            parts.push(name.to_string());
        }
    }
    let key = key_char.map(|c| c.to_string()).unwrap_or_else(|| format!("{:?}", hk.key).replace("Key", "").replace("Digit", ""));
    parts.push(key);
    parts.join(" ")
}

/// Win32 side: layout lookups and the async key scan (no hooks).
pub mod win {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, MapVirtualKeyW, MAPVK_VK_TO_CHAR, MAPVK_VSC_TO_VK};

    /// VK of scan code 0x29 (left of 1) on this thread's keyboard layout.
    pub fn left_of_one_vk() -> Option<u16> {
        // SAFETY: pure table lookup on the current thread's layout.
        let vk = unsafe { MapVirtualKeyW(0x29, MAPVK_VSC_TO_VK) };
        (vk != 0).then_some(vk as u16)
    }

    /// The unshifted character of a VK on this layout (dead keys included).
    pub fn vk_char(vk: u16) -> Option<char> {
        // SAFETY: pure table lookup; the high bit flags a dead key.
        let c = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_CHAR) } & 0x7FFF;
        char::from_u32(c).filter(|c| !c.is_control())
    }

    /// Virtual keys currently held down (polled only while the window is open).
    pub fn keys_down() -> Vec<u16> {
        // SAFETY: GetAsyncKeyState only reads the global async key state.
        (0x08u16..=0xFE).filter(|vk| unsafe { GetAsyncKeyState(*vk as i32) } as u16 & 0x8000 != 0).collect()
    }
}

#[cfg(test)]
#[path = "record_chord_tests.rs"]
mod tests;
