//! What the mic hotkey (Alt+M) does to a strip, beyond its own flag: whether a line-in opens its
//! own saved endpoint (R5-deep-audio-1), the "Muted by the mic hotkey" row (R5-deep-audio-3),
//! the nudge when a line-in is moved onto a microphone (R5-deep-audio-2), and the level its
//! meter shows while muted (R5-deep-audio-5). Pure: lists and states are passed in.
use crate::services::{
    audio_devices::{DeviceLists, Found},
    audio_sources::{Kind, Source},
};

/// The strip note of a line-in the recorder opens on another endpoint than the one it saved.
pub const OPENED_OTHER: &str = "Using a different device: follows the mic hotkey";
/// The menu note of a line-in opened on an unsure sibling (A4 ruling): the recorder adopts no
/// id for a line-in (P1-b), so it stays muted until the user picks the device.
pub const UNSURE_PICK: &str = "May be a different device: stays muted until you pick it";
/// The strip note of a named line-in with no saved endpoint id (a config from before ids).
pub const NO_ID: &str = "Follows the mic hotkey until you pick this device again";
/// The strip note when unticking "Muted by the mic hotkey" was refused: no endpoint id to save.
pub const REFUSED: &str = "Can't read this device's id: pick it again to turn off Muted by the mic hotkey";
/// The strip note when unticking a line-in with no saved id was refused because another
/// present endpoint has the same name once the "N- " index is dropped (identical dongles): the
/// name cannot say which is the line-in, so the user picks it from the list (R7-deep-audio-1).
pub const CONFIRM_PICK: &str = "Pick this device again to confirm it";

/// A line-in (`mic_mute = false`, a named device) that the recorder mutes with Alt+M after all
/// (`recall` `audio/endpoints.rs` `opened_own`, R6-deep-audio-1/2): it saved no well-formed
/// endpoint id (lists not needed), or the endpoint its name opens has another id (a fallback
/// onto another dongle, or a twin that took the name). With an id and lists not read: false.
pub fn opens_other(source: &Source, lists: Option<&DeviceLists>) -> bool {
    other_note(source, lists).is_some()
}

/// The note for [`opens_other`]: [`NO_ID`] or [`OPENED_OTHER`].
pub fn other_note(source: &Source, lists: Option<&DeviceLists>) -> Option<&'static str> {
    use crate::services::meters::{id_ok, opened_own};
    if source.kind != Kind::Input || source.mic_mute != Some(false) || source.device.trim().is_empty() {
        return None;
    }
    if !id_ok(&source.device_id) {
        return Some(NO_ID);
    }
    let lists = lists?;
    match crate::services::audio_pick::source_found(source, Some(lists)) {
        Found::Opens(i) => {
            let id = lists.input_ids.get(i).map(String::as_str).filter(|id| !id.is_empty());
            (!opened_own(id, &source.device, &source.device_id)).then_some(OPENED_OTHER)
        }
        // An unsure sibling (A4 ruling) is never its own, and a line-in never adopts its id
        // (P1-b): only a pick here makes it so.
        Found::Unsure { .. } => Some(UNSURE_PICK),
        _ => None,
    }
}

/// The index in `lists` of the input a pick names: its well-formed endpoint id when listed,
/// else the endpoint its name opens (`match_endpoint`). Read at click time, never kept from a
/// render (R6-deep-audio-4).
fn input_index(lists: &DeviceLists, device: &str, device_id: &str) -> Option<usize> {
    let by_id = crate::services::meters::id_ok(device_id).then(|| lists.input_ids.iter().position(|id| id.trim() == device_id.trim())).flatten();
    by_id.or_else(|| (!device.trim().is_empty()).then(|| crate::services::meters::match_endpoint(&lists.inputs, device)).flatten())
}

/// The "Muted by the mic hotkey" row of an input's menu: (shown checked, what a click saves).
/// With no device it records the Windows default, which Alt+M always mutes: checked and inert,
/// so a click never flips a flag the row cannot show (R5-deep-audio-3). Named: the strip's flag,
/// except a line-in with no saved id, which Alt+M mutes all the same: checked, and a click
/// unticks it, which saves the id (`mic_toggle_effect`, R7-deep-audio-5).
pub fn mic_row(source: &Source) -> (bool, Option<bool>) {
    if source.device.trim().is_empty() {
        return (true, None);
    }
    if source.mic_mute == Some(false) && !crate::services::meters::id_ok(&source.device_id) {
        return (true, Some(false));
    }
    (source.mic_mute != Some(false), Some(source.mic_mute == Some(false)))
}

/// What unticking "Muted by the mic hotkey" saves: (the flag, the endpoint id), as a line-in
/// skips Alt+M only on its saved id (R6-deep-audio-1). The id: the saved one when it is listed,
/// else the one endpoint with exactly the saved name. Refused ([`REFUSED`]) when the lists are
/// not read, the saved id is not listed, the name opens no endpoint or only a renumbered one,
/// or Windows gave no readable id. With no saved id it is also refused ([`CONFIRM_PICK`]) when
/// any other present endpoint has the same name once a leading "N- " index is dropped: two
/// identical dongles, where Windows may have given the saved name to the other one (R7-deep-audio-1).
pub fn mic_toggle_effect(source: &Source, lists: Option<&DeviceLists>) -> Result<(bool, String), &'static str> {
    use crate::services::meters::{id_ok, match_endpoint, stripped_hits};
    let lists = lists.ok_or(REFUSED)?;
    let device = source.device.trim();
    let i = if id_ok(&source.device_id) {
        lists.input_ids.iter().position(|id| id.trim() == source.device_id.trim())
    } else {
        if stripped_hits(&lists.inputs, device) > 1 {
            return Err(CONFIRM_PICK);
        }
        match_endpoint(&lists.inputs, device).filter(|&i| lists.inputs[i].trim() == device)
    };
    let id = i.and_then(|i| lists.input_ids.get(i)).map(|id| id.trim()).filter(|id| id_ok(id)).ok_or(REFUSED)?;
    Ok((false, id.to_owned()))
}

/// Windows calls this form factor a microphone (`EndpointFormFactor` Microphone 4, Headset 5,
/// Handset 6). A page-side nudge only; the recorder never decides by form factor.
pub fn is_microphone(form: Option<u32>) -> bool {
    matches!(form, Some(4..=6))
}

/// The flag to save when `source` picks the input `(device, device_id)` (`None` = leave it): a
/// line-in moved onto an endpoint Windows lists as a microphone gets Alt+M back on, in the safe
/// direction; the page shows a toast and the user can turn it off again (R5-deep-audio-2). The
/// device is found in `lists` as they are at the click (R6-deep-audio-4).
pub fn device_pick_effect(source: &Source, lists: Option<&DeviceLists>, (device, device_id): (&str, &str)) -> Option<bool> {
    let form = lists.and_then(|l| input_index(l, device, device_id).and_then(|i| l.input_forms.get(i).copied().flatten()));
    (source.kind == Kind::Input && source.mic_mute == Some(false) && is_microphone(form)).then_some(true)
}

/// The toast when `device_pick_effect` turned Alt+M back on.
pub const REARMED: &str = "Muted by the mic hotkey is on again: Windows lists this device as a microphone";

/// The level a strip's meter shows: silence while Alt+M is on (`muted_now`) and it mutes this
/// source (`mutes`), as that is what the clip records (R5-deep-audio-5).
pub fn shown_level(db: f32, muted_now: bool, mutes: bool) -> f32 {
    if muted_now && mutes { -90.0 } else { db }
}

/// The recorder holds this input silent whatever Alt+M says: it opens an unsure same-named
/// sibling (A4 ruling, `recall` `audio/hold.rs`): a microphone until a press unmutes it and
/// its id is saved, a line-in until the user picks the device (it never adopts, P1-b).
/// `engine`: what the recorder says for this source (`status.audio[].held`, merge-gate P2), which
/// wins over the guess: a release the device lists have not caught up with, or a hold they
/// cannot see. None (an older recorder, or no status yet): the guess from the device lists.
pub fn held(source: &Source, lists: Option<&DeviceLists>, engine: Option<bool>) -> bool {
    engine.unwrap_or_else(|| source.kind.uses_inputs() && matches!(crate::services::audio_pick::source_found(source, lists), Found::Unsure { .. }))
}

/// What the recorder says for one source (`status.audio[]`): both ids trimmed the same way, as
/// the engine trims the config's (controller review P3-1).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EngineMeter {
    pub held: Option<bool>,
    pub mutes: Option<bool>,
    pub retired: bool,
}

pub fn engine_meter(audio: &[crate::services::daemon::EngineAudio], id: &str) -> EngineMeter {
    audio.iter().find(|a| a.id.trim() == id.trim()).map_or_else(EngineMeter::default, |a| EngineMeter { held: a.held, mutes: a.mutes, retired: a.retired == Some(true) })
}

/// The meter's "muted now" and "Alt+M mutes it" (design-audio-mute P20): the recorder's own
/// `mutes` when it says, so the meter never shows "Muted" for a track it records live; the
/// page's guess (`session_muted`, `mutes`) only from a recorder that does not say.
pub fn muting(engine: &EngineMeter, session_muted: bool, mutes: bool) -> (bool, bool) {
    engine.mutes.map_or((session_muted, mutes), |m| (m, true))
}

/// A retired source's strip (P11, P16): it records nothing until the recording ends.
pub const RETIRED: &str = "Off: applies after the recording";
/// The meter's text once the recorder's state is known: a retired source says when it applies.
pub fn meter_text(text: String, engine: &EngineMeter) -> String {
    if engine.retired { RETIRED.to_owned() } else { text }
}

/// The meter's bar, hold line and number for a strip (levels after the gain): flat while Alt+M
/// mutes the source, or while the recorder holds it ([`held`], gate review P2-e), as that is
/// what the clip records. "—" when the meter has no live reading.
pub fn meter_reading((rms, hold, live): (f32, f32, bool), muted_now: bool, mutes: bool, held: bool) -> (f32, f32, String) {
    let flat = held || (muted_now && mutes);
    let (rms, hold) = if held { (-90.0, -90.0) } else { (shown_level(rms, muted_now, mutes), shown_level(hold, muted_now, mutes)) };
    let text = match () {
        _ if !live => "—".to_owned(),
        _ if held => "Held".to_owned(),
        _ if flat => "Muted".to_owned(),
        _ if rms <= -89.0 => "-∞".to_owned(),
        _ => format!("{rms:.0} dB"),
    };
    (rms, hold, text)
}

#[cfg(test)]
#[path = "audio_mute_tests.rs"]
mod tests;
