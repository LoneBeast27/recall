//! The record mode's hotkey set, as data: what each accelerator does, the
//! save-length presets (`recording.save_hotkeys`) with the legacy single
//! `save_hotkey` kept working, and conflict detection.
//!
//! - **Defaults**: one direct Alt combo per action -- Alt+N save, Alt+M mic,
//!   Alt+B bookmark, Alt+S screenshot, Alt+L long recording, Alt+P stats,
//!   Alt+O overlay (notices) on/off.
//!   Rohan, 2026-11-05: the two-step chord (Alt+` then a digit) was on Alt
//!   anyway, so it is gone. [`GAMER_COMBOS`] are never defaults. Bookmark,
//!   screenshot, record and stats can be turned off ("").
//! - **Chord-era configs.** A file that still has the old `[hotkeys]` table
//!   gets the new defaults for any of those four left empty (once: the next
//!   save drops the table), unless another binding already holds that combo.
//! - **Presets.** `[[recording.save_hotkeys]] hotkey = "Alt+N" seconds = 0`
//!   (0 = the whole buffer); more presets are user-added.
//! - **Legacy key.** `save_hotkey` always mirrors the first full-buffer preset.
//!   A config file without `save_hotkeys` is migrated from its `save_hotkey`;
//!   a patch / env override that sets only `save_hotkey` moves that preset.
//! - **Conflicts.** `validate` rejects a config whose hotkeys collide (patches
//!   fail with "must differ"); at bind time collisions and OS refusals become
//!   warnings (`session` error events), the first binding wins, never fatal.

use std::collections::HashMap;

use global_hotkey::hotkey::HotKey;
use serde_json::Value;

use crate::hotkey::parse_accel;
use crate::record_config::{
RecordingCfg, SaveHotkey, BUFFER_SECS_MAX};

pub const MAX_SAVE_HOTKEYS: usize = 8;

pub const DEFAULT_BOOKMARK: &str = "Alt+B";
pub const DEFAULT_SCREENSHOT: &str = "Alt+S";
pub const DEFAULT_RECORD: &str = "Alt+L";
pub const DEFAULT_STATS: &str = "Alt+P";
pub const DEFAULT_OVERLAY: &str = "Alt+O";

/// Combos games, launchers and overlays commonly own (GeForce/ShadowPlay,
/// Xbox Game Bar, Steam, Discord, screenshot tools), plus Ctrl+Alt+R (a
/// combo the user rejected). Never a default. `Win+Alt+*` is
/// checked by modifier, not listed.
pub const GAMER_COMBOS: [&str; 13] = [
    "Alt+R", "Alt+Z", "Alt+F1", "Alt+F9", "Alt+F10", "Ctrl+Shift+S", "Ctrl+Shift+E", "Ctrl+Shift+I", "F8", "F12",
    "Shift+Tab", "Alt+Tab", "Ctrl+Alt+R",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    /// Save `seconds` of the buffer (0 = all of it).
    Save { seconds: u32 },
    Mic,
    Bookmark,
    Screenshot,
    Record,
    /// Show / hide the performance-stats island.
    Stats,
    /// Turn the overlay's notices (`overlay.enabled`) on or off.
    Overlay,
    ToggleHdr,
}

/// One accelerator to register.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub action: HotkeyAction,
    /// Config path, for validation errors (`recording.save_hotkeys[1]`).
    pub key: String,
    /// Human label for warnings (`save (last 30 s)`).
    pub label: String,
    pub accel: String,
}

pub fn default_save_hotkeys() -> Vec<SaveHotkey> {
    vec![SaveHotkey { hotkey: "Alt+N".into(), seconds: 0 }]
}

pub fn save_label(seconds: u32) -> String {
    if seconds == 0 {
        "save (full buffer)".into()
    } else {
        format!("save (last {seconds} s)")
    }
}

/// Every configured accelerator (empty = disabled), saves first; `hdr` is the
/// HDR toggle while HDR management is on.
pub fn bindings(rec: &RecordingCfg, hdr: Option<&str>) -> Vec<Binding> {
    let mut out: Vec<Binding> = rec
        .save_hotkeys
        .iter()
        .enumerate()
        .map(|(i, s)| Binding {
            action: HotkeyAction::Save { seconds: s.seconds },
            key: format!("recording.save_hotkeys[{i}]"),
            label: save_label(s.seconds),
            accel: s.hotkey.clone(),
        })
        .collect();
    let singles = [
        (HotkeyAction::Mic, "recording.mic_hotkey", "microphone", rec.mic_hotkey.as_str()),
        (HotkeyAction::Bookmark, "recording.bookmark_hotkey", "bookmark", rec.bookmark_hotkey.as_str()),
        (HotkeyAction::Screenshot, "recording.screenshot_hotkey", "screenshot", rec.screenshot_hotkey.as_str()),
        (HotkeyAction::Record, "recording.record_hotkey", "long recording", rec.record_hotkey.as_str()),
        (HotkeyAction::Stats, "recording.stats_hotkey", "performance stats", rec.stats_hotkey.as_str()),
        (HotkeyAction::Overlay, "recording.overlay_hotkey", "overlay", rec.overlay_hotkey.as_str()),
        (HotkeyAction::ToggleHdr, "hotkey.toggle_hdr", "HDR toggle", hdr.unwrap_or("")),
    ];
    for (action, key, label, accel) in singles {
        out.push(Binding { action, key: key.into(), label: label.into(), accel: accel.into() });
    }
    out.retain(|b| !b.accel.trim().is_empty());
    out
}

/// Parse every binding; the first holder of an accelerator keeps it. Returns
/// the registrable bindings and one warning per invalid or conflicting one.
pub fn split_conflicts(list: Vec<Binding>) -> (Vec<(Binding, HotKey)>, Vec<String>) {
    let mut owners: HashMap<u32, String> = HashMap::new();
    let mut keep = Vec::new();
    let mut warnings = Vec::new();
    for b in list {
        match parse_accel(&b.accel) {
            Err(e) => warnings.push(format!("{} hotkey is not registered: {e}", b.label)),
            Ok(hk) => match owners.get(&hk.id()) {
                Some(owner) => warnings.push(format!(
                    "{} hotkey '{}' conflicts with the {owner} hotkey; only {owner} uses it",
                    b.label, b.accel
                )),
                None => {
                    owners.insert(hk.id(), b.label.clone());
                    keep.push((b, hk));
                }
            },
        }
    }
    (keep, warnings)
}

/// Config validation: presets bounded, every accelerator parses, none collide
/// (the HDR toggle is checked at bind time only, when it is bound).
pub fn validate(rec: &RecordingCfg) -> Result<(), String> {
    if rec.save_hotkeys.len() > MAX_SAVE_HOTKEYS {
        return Err(format!("recording.save_hotkeys allows at most {MAX_SAVE_HOTKEYS} presets"));
    }
    for (i, s) in rec.save_hotkeys.iter().enumerate() {
        if s.hotkey.trim().is_empty() {
            return Err(format!("recording.save_hotkeys[{i}].hotkey must not be empty"));
        }
        if s.seconds > BUFFER_SECS_MAX {
            return Err(format!("recording.save_hotkeys[{i}].seconds must be 0 (full buffer) ..={BUFFER_SECS_MAX}"));
        }
    }
    if rec.mic_hotkey.trim().is_empty() {
        return Err("recording.mic_hotkey must not be empty".into());
    }
    let mut owners: HashMap<u32, String> = HashMap::new();
    for b in bindings(rec, None) {
        let hk = parse_accel(&b.accel).map_err(|e| format!("{}: {e}", b.key))?;
        if let Some(owner) = owners.insert(hk.id(), b.label.clone()) {
            return Err(format!(
                "recording hotkeys must differ: '{}' is used by both {owner} and {}",
                b.accel, b.label
            ));
        }
    }
    Ok(())
}

/// Whether `accel` is one of [`GAMER_COMBOS`] or any `Win+Alt+*` chord.
pub fn is_gamer_combo(accel: &str) -> bool {
    use global_hotkey::hotkey::Modifiers;
    let Ok(hk) = parse_accel(accel) else { return false };
    let win_alt = hk.mods.contains(Modifiers::SUPER) && hk.mods.contains(Modifiers::ALT);
    win_alt || GAMER_COMBOS.iter().filter_map(|c| parse_accel(c).ok()).any(|c| c.id() == hk.id())
}

/// The legacy `save_hotkey` value: the first full-buffer preset, else the
/// first preset, else "".
pub fn legacy_save_hotkey(list: &[SaveHotkey]) -> String {
    list.iter()
        .find(|s| s.seconds == 0)
        .or_else(|| list.first())
        .map(|s| s.hotkey.clone())
        .unwrap_or_default()
}

/// `list` with the full-buffer preset's accelerator set to `accel` (inserted
/// first when there is none).
pub fn with_full_buffer_hotkey(list: &[SaveHotkey], accel: &str) -> Vec<SaveHotkey> {
    let mut out = list.to_vec();
    match out.iter_mut().find(|s| s.seconds == 0) {
        Some(full) => full.hotkey = accel.to_string(),
        None => out.insert(0, SaveHotkey { hotkey: accel.to_string(), seconds: 0 }),
    }
    out
}

/// Presets for a config that only has the legacy key: it saves the full
/// buffer.
pub fn migrated(rec: &RecordingCfg) -> Vec<SaveHotkey> {
    let legacy = if rec.save_hotkey.trim().is_empty() { "Alt+N" } else { rec.save_hotkey.trim() };
    vec![SaveHotkey { hotkey: legacy.to_string(), seconds: 0 }]
}

/// After loading `toml_text`: migrate a `[recording]` without `save_hotkeys`
/// from its legacy key, re-derive the legacy mirror, and give a chord-era
/// file (it still has `[hotkeys]`) the direct defaults it left empty.
pub fn migrate_loaded(rec: &mut RecordingCfg, toml_text: &str) {
    let doc = toml::from_str::<toml::Value>(toml_text).ok();
    let has_presets =
        doc.as_ref().and_then(|v| v.get("recording").map(|r| r.get("save_hotkeys").is_some())).unwrap_or(false);
    if !has_presets {
        rec.save_hotkeys = migrated(rec);
    }
    rec.save_hotkey = legacy_save_hotkey(&rec.save_hotkeys);
    if doc.as_ref().is_some_and(|v| v.get("hotkeys").is_some()) {
        fill_chord_era_defaults(rec);
    }
    let has = |key: &str| doc.as_ref().and_then(|v| v.get("recording")).is_some_and(|r| r.get(key).is_some());
    if !has("stats_hotkey") {
        drop_taken_default(rec, HotkeyAction::Stats);
    }
    if !has("overlay_hotkey") {
        drop_taken_default(rec, HotkeyAction::Overlay);
    }
}

/// A key the file did not have came in as its serde default: turn it off
/// ("") when another binding already holds that combo, so the loaded config
/// still validates and Settings can save.
fn drop_taken_default(rec: &mut RecordingCfg, action: HotkeyAction) {
    let id = |accel: &str| parse_accel(accel).ok().map(|h| h.id());
    let list = bindings(rec, None);
    let Some(mine) = list.iter().find(|b| b.action == action).and_then(|b| id(&b.accel)) else { return };
    let taken = list.iter().any(|b| b.action != action && id(&b.accel) == Some(mine));
    if taken {
        match action {
            HotkeyAction::Stats => rec.stats_hotkey.clear(),
            HotkeyAction::Overlay => rec.overlay_hotkey.clear(),
            _ => {}
        }
    }
}

/// Each empty bookmark / screenshot / record / stats key takes its default
/// unless an existing binding already holds that combo.
fn fill_chord_era_defaults(rec: &mut RecordingCfg) {
    let held: Vec<u32> = bindings(rec, None).iter().filter_map(|b| parse_accel(&b.accel).ok()).map(|h| h.id()).collect();
    let slots = [
        (&mut rec.bookmark_hotkey, DEFAULT_BOOKMARK),
        (&mut rec.screenshot_hotkey, DEFAULT_SCREENSHOT),
        (&mut rec.record_hotkey, DEFAULT_RECORD),
        (&mut rec.stats_hotkey, DEFAULT_STATS),
    ];
    for (slot, default) in slots {
        let free = parse_accel(default).is_ok_and(|h| !held.contains(&h.id()));
        if slot.trim().is_empty() && free {
            *slot = default.to_string();
        }
    }
}

/// A patch that sets `recording.save_hotkey` but not `save_hotkeys` becomes a
/// `save_hotkeys` patch (the full-buffer preset moves). When both are set the
/// list wins. Anything else passes through unchanged.
pub fn rewrite_legacy_patch(base: &RecordingCfg, patch: &Value) -> Value {
    let mut patch = patch.clone();
    let Some(rec) = patch.get_mut("recording").and_then(Value::as_object_mut) else { return patch };
    if rec.contains_key("save_hotkeys") {
        rec.remove("save_hotkey");
        return patch;
    }
    if let Some(Value::String(accel)) = rec.get("save_hotkey").cloned() {
        rec.remove("save_hotkey");
        let list = with_full_buffer_hotkey(&base.save_hotkeys, accel.trim());
        rec.insert("save_hotkeys".into(), serde_json::to_value(list).unwrap_or(Value::Null));
    }
    patch
}

#[cfg(test)]
#[path = "record_hotkeys_tests.rs"]
mod tests;
