//! What the mic hotkey (Alt+M) does to a strip, beyond its own flag: whether a line-in opens its
//! own saved endpoint (R5-deep-audio-1), the "Muted by the mic hotkey" row (R5-deep-audio-3),
//! the nudge when a line-in is moved onto a microphone (R5-deep-audio-2), and the level its
//! meter shows while muted (R5-deep-audio-5). Pure: lists and states are passed in.
use crate::services::{
    audio_devices::{DeviceLists, Found, found},
    audio_sources::{Kind, Source},
};

/// The strip note of a line-in on another endpoint than the one it saved.
pub const OPENED_OTHER: &str = "Opened a different device: follows the mic hotkey";

/// A line-in (`mic_mute = false`, a named device) that the recorder opens on another endpoint
/// than its own: a renumbered fallback ("Mic (Box)" saved, only "2- Mic (Box)" there), or the
/// saved name on an endpoint with another id (a twin took it). The recorder mutes it with Alt+M
/// (`recall` `audio/endpoints.rs` `opened_own`); the page says so. Lists not read yet: false.
pub fn opens_other(source: &Source, lists: Option<&DeviceLists>) -> bool {
    let Some(lists) = lists.filter(|_| source.kind == Kind::Input && source.mic_mute == Some(false)) else { return false };
    match found(&source.device, Some(&lists.inputs)) {
        Found::Opens(i) => {
            let id = lists.input_ids.get(i).map(String::as_str).filter(|id| !id.is_empty());
            !crate::services::meters::opened_own(&lists.inputs[i], id, &source.device, &source.device_id)
        }
        _ => false,
    }
}

/// The "Muted by the mic hotkey" row of an input's menu: (shown checked, what a click saves).
/// With no device it records the Windows default, which Alt+M always mutes: checked and inert,
/// so a click never flips a flag the row cannot show (R5-deep-audio-3). Named: the strip's flag.
pub fn mic_row(source: &Source) -> (bool, Option<bool>) {
    if source.device.trim().is_empty() {
        return (true, None);
    }
    (source.mic_mute != Some(false), Some(source.mic_mute == Some(false)))
}

/// Windows calls this form factor a microphone (`EndpointFormFactor` Microphone 4, Headset 5,
/// Handset 6). A page-side nudge only; the recorder never decides by form factor.
pub fn is_microphone(form: Option<u32>) -> bool {
    matches!(form, Some(4..=6))
}

/// The flag to save when `source` picks the input at `index` of `lists` (`None` = leave it): a
/// line-in moved onto an endpoint Windows lists as a microphone gets Alt+M back on, in the safe
/// direction; the page shows a toast and the user can turn it off again (R5-deep-audio-2).
pub fn device_pick_effect(source: &Source, lists: Option<&DeviceLists>, index: Option<usize>) -> Option<bool> {
    let form = lists.zip(index).and_then(|(l, i)| l.input_forms.get(i).copied().flatten());
    (source.kind == Kind::Input && source.mic_mute == Some(false) && is_microphone(form)).then_some(true)
}

/// The toast when `device_pick_effect` turned Alt+M back on.
pub const REARMED: &str = "Muted by the mic hotkey is on again: Windows lists this device as a microphone";

/// The level a strip's meter shows: silence while Alt+M is on (`muted_now`) and it mutes this
/// source (`mutes`), as that is what the clip records (R5-deep-audio-5).
pub fn shown_level(db: f32, muted_now: bool, mutes: bool) -> f32 {
    if muted_now && mutes { -90.0 } else { db }
}

#[cfg(test)]
#[path = "audio_mute_tests.rs"]
mod tests;
