//! Settings → Shortcuts: click a key chip, press the new keys. Esc cancels; Backspace turns an
//! optional shortcut off. The chord goes to the controller as `set_config`; a clash or a key
//! it cannot bind comes back as an error toast and nothing changes.
use gpui::{AnyElement, Context, Keystroke, SharedString, div, prelude::*, px};
use serde_json::{Value, json};

use crate::{
    root::Root,
    theme::{self, c},
    ui,
};

/// A rebindable shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutSlot {
    Save,
    Mic,
    Bookmark,
    Screenshot,
    Record,
    Stats,
    Overlay,
    Editor,
}

impl ShortcutSlot {
    pub const ALL: [ShortcutSlot; 8] = [
        ShortcutSlot::Save,
        ShortcutSlot::Mic,
        ShortcutSlot::Bookmark,
        ShortcutSlot::Screenshot,
        ShortcutSlot::Record,
        ShortcutSlot::Stats,
        ShortcutSlot::Overlay,
        ShortcutSlot::Editor,
    ];

    /// The controller config key: (section, key).
    pub(crate) fn config_key(self) -> (&'static str, &'static str) {
        self.key()
    }

    fn key(self) -> (&'static str, &'static str) {
        match self {
            ShortcutSlot::Save => ("recording", "save_hotkey"),
            ShortcutSlot::Mic => ("recording", "mic_hotkey"),
            ShortcutSlot::Bookmark => ("recording", "bookmark_hotkey"),
            ShortcutSlot::Screenshot => ("recording", "screenshot_hotkey"),
            ShortcutSlot::Record => ("recording", "record_hotkey"),
            ShortcutSlot::Stats => ("recording", "stats_hotkey"),
            ShortcutSlot::Overlay => ("recording", "overlay_hotkey"),
            ShortcutSlot::Editor => ("recording", "editor_hotkey"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ShortcutSlot::Save => "Save replay",
            ShortcutSlot::Mic => "Mute microphone in clips",
            ShortcutSlot::Bookmark => "Bookmark",
            ShortcutSlot::Screenshot => "Screenshot",
            ShortcutSlot::Record => "Start or stop a recording",
            ShortcutSlot::Stats => "Performance stats",
            ShortcutSlot::Overlay => "Show or hide the overlay",
            ShortcutSlot::Editor => "Place the island for this game",
        }
    }

    /// The slot whose config path is `key` (`recording.mic_hotkey`). The save chip edits the
    /// full-buffer preset, so `recording.save_hotkeys[i]` is it only when `accel` is that
    /// preset's combo (`save_accel`).
    pub fn from_config_key(key: &str, accel: &str, save_accel: &str) -> Option<ShortcutSlot> {
        if key.starts_with("recording.save_hotkeys[") {
            return accel.eq_ignore_ascii_case(save_accel).then_some(ShortcutSlot::Save);
        }
        ShortcutSlot::ALL.into_iter().find(|s| s != &ShortcutSlot::Save && format!("recording.{}", s.key().1) == key)
    }

    /// Optional shortcuts can be off ("" in config); the others always have keys.
    pub(crate) fn optional(self) -> bool {
        matches!(self, ShortcutSlot::Bookmark | ShortcutSlot::Screenshot | ShortcutSlot::Record | ShortcutSlot::Stats | ShortcutSlot::Overlay | ShortcutSlot::Editor)
    }
}

/// What one key press does while a chip waits for keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capture {
    /// A modifier on its own: keep waiting.
    Wait,
    Cancel,
    /// Send this accelerator ("" = off).
    Set(String),
    /// A combo Windows owns: keep waiting and say why.
    Refuse(String),
}

/// Combos Windows acts on in every window, in [`accelerator`]'s spelling, that the chip can
/// catch (Esc and Delete chords never reach it). The recorder's `hotkey::WINDOWS_OWNED` is the
/// full list; it never registers any of them.
const WINDOWS_KEEPS: [(&str, &str); 4] = [
    ("Alt+F4", "it closes the window in front"),
    ("Alt+Tab", "it switches windows"),
    ("Alt+Shift+Tab", "it switches windows"),
    ("Alt+Space", "it opens the window menu"),
];

/// Why Windows keeps `accel`, if it does.
pub fn windows_keeps(accel: &str) -> Option<&'static str> {
    WINDOWS_KEEPS.iter().find(|(combo, _)| combo.eq_ignore_ascii_case(accel)).map(|(_, why)| *why)
}

/// The one-line note under a bare F-key: it works, but games stop getting that key.
pub fn bare_fkey_note(accel: &str) -> Option<String> {
    let n = accel.strip_prefix('F').or_else(|| accel.strip_prefix('f'))?;
    n.parse::<u8>().is_ok().then(|| format!("On its own, {} stops reaching games while Recall runs", accel.to_ascii_uppercase()))
}

/// Decide what a key press means for `slot` (pure, so it is tested without a window).
pub fn capture(slot: ShortcutSlot, k: &Keystroke) -> Capture {
    let m = &k.modifiers;
    let any_mod = m.control || m.alt || m.shift || m.platform;
    match k.key.as_str() {
        "escape" if !any_mod => return Capture::Cancel,
        "backspace" | "delete" if !any_mod => return if slot.optional() { Capture::Set(String::new()) } else { Capture::Wait },
        _ => {}
    }
    match accelerator(&k.key, m.control, m.alt, m.shift, m.platform) {
        Some(text) => match windows_keeps(&text) {
            Some(why) => Capture::Refuse(format!("Windows keeps {text} ({why}). Pick another key.")),
            None => Capture::Set(text),
        },
        None => Capture::Wait,
    }
}

/// gpui key + modifiers → the controller's accelerator text ("Ctrl+Alt+F7", "Alt+Backquote").
/// `None` for modifier keys alone, and for a plain letter or digit without a modifier (it
/// would take that key from every game). F-keys may stand alone.
pub fn accelerator(key: &str, ctrl: bool, alt: bool, shift: bool, win: bool) -> Option<String> {
    let token = key_token(key)?;
    let is_fkey = token.len() > 1 && token.starts_with('F') && token[1..].parse::<u8>().is_ok();
    if !(ctrl || alt || shift || win) && !is_fkey {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for (on, name) in [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (win, "Win")] {
        if on {
            parts.push(name);
        }
    }
    parts.push(&token);
    Some(parts.join("+"))
}

fn key_token(key: &str) -> Option<String> {
    let named = match key {
        "`" => "Backquote",
        "-" => "Minus",
        "=" => "Equal",
        "[" => "BracketLeft",
        "]" => "BracketRight",
        "\\" => "Backslash",
        ";" => "Semicolon",
        "'" => "Quote",
        "," => "Comma",
        "." => "Period",
        "/" => "Slash",
        "space" => "Space",
        "enter" => "Enter",
        "tab" => "Tab",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_owned());
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) if ch.is_ascii_alphanumeric() => Some(ch.to_ascii_uppercase().to_string()),
        (Some('f'), Some(_)) if key[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => Some(key.to_ascii_uppercase()),
        _ => None,
    }
}

/// Chip text for an accelerator: "Alt+Backquote" → "Alt `".
pub fn chip_text(accel: &str) -> String {
    accel.split('+').map(|t| if t.eq_ignore_ascii_case("backquote") { "`" } else { t }).collect::<Vec<_>>().join(" ")
}

impl Root {
    /// The saved accelerator for a slot (live config first, then the last status).
    pub(crate) fn shortcut_value(&self, slot: ShortcutSlot) -> String {
        let (section, key) = slot.key();
        if let Some(v) = self.config_value(section, key).and_then(Value::as_str) {
            return v.to_owned();
        }
        let s = self.snap.session.clone().unwrap_or_default();
        match slot {
            ShortcutSlot::Save => s.save_hotkey,
            ShortcutSlot::Mic => s.mic_hotkey,
            ShortcutSlot::Bookmark => s.bookmark_hotkey,
            ShortcutSlot::Screenshot => s.screenshot_hotkey,
            ShortcutSlot::Record => s.record_hotkey,
            ShortcutSlot::Stats => s.stats_hotkey,
            ShortcutSlot::Overlay => s.overlay_hotkey,
            ShortcutSlot::Editor => s.editor_hotkey,
        }
    }

    /// One "label · key chip" row; clicking the chip waits for new keys.
    pub fn shortcut_row(&self, slot: ShortcutSlot, cx: &mut Context<Self>) -> AnyElement {
        let waiting = self.settings_ui.capture == Some(slot);
        let value = self.shortcut_value(slot);
        let chip = if waiting {
            ui::kbd("Press keys…").border_color(c(theme::FOCUS))
        } else if value.is_empty() {
            ui::kbd("Not set").text_color(c(theme::TERTIARY))
        } else {
            ui::kbd(chip_text(&value))
        };
        let hint = if waiting {
            match &self.settings_ui.capture_note {
                Some(note) => note.clone(),
                None if slot.optional() => "Esc cancels · Backspace turns it off".to_owned(),
                None => "Esc cancels".to_owned(),
            }
        } else {
            bare_fkey_note(&value).unwrap_or_default()
        };
        div()
            .h(px(34.))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(c(theme::BORDER_SUBTLE))
            .child(div().child(slot.label()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(hint))
                    .child(
                        div()
                            .id(SharedString::from(format!("shortcut-{}", slot.key().1)))
                            .cursor_pointer()
                            .child(chip)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.settings_ui.capture = if this.settings_ui.capture == Some(slot) { None } else { Some(slot) };
                                this.settings_ui.capture_note = None;
                                window.focus(&this.focus);
                                cx.notify();
                            }))
                            .when(waiting, |d| {
                                d.on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                    this.settings_ui.capture = None;
                                    cx.notify();
                                }))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// A key press while a chip waits. Returns true when the press was taken.
    pub fn capture_shortcut_key(&mut self, k: &Keystroke) -> bool {
        let Some(slot) = self.settings_ui.capture else { return false };
        match capture(slot, k) {
            Capture::Wait => {}
            Capture::Refuse(note) => self.settings_ui.capture_note = Some(note),
            Capture::Cancel => {
                self.settings_ui.capture = None;
                self.settings_ui.capture_note = None;
            }
            Capture::Set(accel) => {
                self.settings_ui.capture = None;
                self.settings_ui.capture_note = None;
                let old = self.shortcut_value(slot);
                if accel != old {
                    // Leaving a combo Windows owns (the fix's "Pick another"): offer Undo.
                    if self.owned_slot(slot).is_some() {
                        self.settings_ui.shortcut_undo = Some((slot, old));
                    }
                    let (section, key) = slot.key();
                    self.set_config(section, key, json!(accel));
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: &str, ctrl: bool, alt: bool, shift: bool) -> Keystroke {
        let mut k = Keystroke::parse(key).unwrap();
        k.modifiers.control = ctrl;
        k.modifiers.alt = alt;
        k.modifiers.shift = shift;
        k
    }

    #[test]
    fn chords_become_controller_accelerators() {
        assert_eq!(accelerator("n", false, true, false, false).as_deref(), Some("Alt+N"));
        assert_eq!(accelerator("`", false, true, false, false).as_deref(), Some("Alt+Backquote"));
        assert_eq!(accelerator("f7", true, false, true, false).as_deref(), Some("Ctrl+Shift+F7"));
        assert_eq!(accelerator("f9", false, false, false, false).as_deref(), Some("F9"), "F-keys may stand alone");
        assert_eq!(accelerator("k", false, false, false, true).as_deref(), Some("Win+K"));
        assert_eq!(accelerator("n", false, false, false, false), None, "a bare letter would take it from games");
        assert_eq!(accelerator("shift", false, false, true, false), None, "modifier alone: keep waiting");
        assert_eq!(accelerator("f", false, false, false, false), None);
    }

    #[test]
    fn every_function_key_works_alone_and_in_a_chord() {
        for n in 1..=24 {
            let key = format!("f{n}");
            assert_eq!(accelerator(&key, false, false, false, false), Some(format!("F{n}")), "{key}");
            assert_eq!(accelerator(&key, false, true, false, false), Some(format!("Alt+F{n}")), "{key}");
        }
        assert_eq!(capture(ShortcutSlot::Save, &press("f10", false, true, false)), Capture::Set("Alt+F10".into()));
        assert_eq!(capture(ShortcutSlot::Save, &press("f10", false, false, false)), Capture::Set("F10".into()));
        assert_eq!(capture(ShortcutSlot::Save, &press("f9", true, false, false)), Capture::Set("Ctrl+F9".into()));
        assert_eq!(accelerator("f25", false, true, false, false), None, "Windows has no F25");
        assert_eq!(chip_text("Alt+F10"), "Alt F10");
    }

    #[test]
    fn the_picker_refuses_combos_windows_owns_and_keeps_waiting() {
        let Capture::Refuse(note) = capture(ShortcutSlot::Screenshot, &press("f4", false, true, false)) else { panic!("Alt+F4 must be refused") };
        assert!(note.contains("Alt+F4") && note.contains("closes the window in front"), "{note}");
        assert!(matches!(capture(ShortcutSlot::Save, &press("tab", false, true, false)), Capture::Refuse(_)));
        assert!(matches!(capture(ShortcutSlot::Save, &press("tab", false, true, true)), Capture::Refuse(_)));
        assert!(matches!(capture(ShortcutSlot::Save, &press("space", false, true, false)), Capture::Refuse(_)));
        assert_eq!(capture(ShortcutSlot::Save, &press("f4", true, false, false)), Capture::Set("Ctrl+F4".into()), "only the exact combo");
        assert_eq!(capture(ShortcutSlot::Save, &press("f4", false, true, true)), Capture::Set("Alt+Shift+F4".into()));
    }

    #[test]
    fn a_bare_function_key_gets_a_note_and_chords_do_not() {
        assert!(bare_fkey_note("F10").is_some_and(|n| n.contains("F10") && n.contains("games")));
        assert!(bare_fkey_note("F24").is_some());
        assert_eq!(bare_fkey_note("Alt+F10"), None);
        assert_eq!(bare_fkey_note("Alt+N"), None);
        assert_eq!(bare_fkey_note(""), None);
        assert_eq!(bare_fkey_note("Fx"), None);
    }

    #[test]
    fn windows_owned_entries_map_to_their_chip() {
        assert_eq!(ShortcutSlot::from_config_key("recording.screenshot_hotkey", "Alt+F4", "Alt+N"), Some(ShortcutSlot::Screenshot));
        assert_eq!(ShortcutSlot::from_config_key("recording.mic_hotkey", "Alt+Tab", "Alt+N"), Some(ShortcutSlot::Mic));
        assert_eq!(ShortcutSlot::from_config_key("recording.save_hotkeys[0]", "Alt+F4", "alt+f4"), Some(ShortcutSlot::Save));
        assert_eq!(ShortcutSlot::from_config_key("recording.save_hotkeys[1]", "Alt+Tab", "Alt+N"), None, "a second preset has no chip");
        assert_eq!(ShortcutSlot::from_config_key("recording.save_hotkey", "Alt+F4", "Alt+N"), None);
    }

    #[test]
    fn esc_cancels_and_backspace_clears_only_optional_shortcuts() {
        let esc = press("escape", false, false, false);
        let back = press("backspace", false, false, false);
        assert_eq!(capture(ShortcutSlot::Save, &esc), Capture::Cancel);
        assert_eq!(capture(ShortcutSlot::Bookmark, &back), Capture::Set(String::new()));
        assert_eq!(capture(ShortcutSlot::Mic, &back), Capture::Wait, "the mic shortcut must keep a key");
        assert_eq!(capture(ShortcutSlot::Save, &press("j", false, true, false)), Capture::Set("Alt+J".into()));
        assert_eq!(capture(ShortcutSlot::Stats, &press("alt", false, true, false)), Capture::Wait, "modifier alone: keep waiting");
        assert_eq!(capture(ShortcutSlot::Stats, &back), Capture::Set(String::new()), "stats can be turned off");
    }

    #[test]
    fn chips_show_the_backquote_as_its_key() {
        assert_eq!(chip_text("Alt+Backquote"), "Alt `");
        assert_eq!(chip_text("Ctrl+F7"), "Ctrl F7");
    }
}
