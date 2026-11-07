//! Audio endpoints for Settings → Clip → Audio (Microphone, Desktop audio) and MIDI inputs for
//! Settings → General. Listed lazily: the first time either tab opens, on a thread of its own (COM lives and dies there), and
//! again each time Settings opens (`stale`). Nothing here runs before first paint.
use wasapi::{DeviceCollection, Direction};

/// Active endpoints by friendly name, as Windows shows them (and as the engine matches them).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceLists {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub default_input: Option<String>,
    pub default_output: Option<String>,
    /// The inputs whose Windows form factor is a microphone, headset or handset
    /// (`endpoint_form`): Alt+M mutes them unless their strip says otherwise.
    pub mic_inputs: Vec<String>,
    /// The default input is such a microphone (`None`: there is none).
    pub default_input_is_mic: Option<bool>,
    /// MIDI input ports (Settings → General → MIDI device).
    pub midi_inputs: Vec<String>,
}

/// Load state; the list is fetched on first draw and kept until Settings opens again.
#[derive(Debug, Default)]
pub enum AudioDevices {
    #[default]
    NotLoaded,
    Loading,
    Ready(DeviceLists),
}

impl AudioDevices {
    /// Settings opened again: a loaded list is read afresh on its next draw (a MIDI device
    /// plugged in since shows up). A list still loading is kept.
    pub fn stale(&mut self) {
        if matches!(self, AudioDevices::Ready(_)) {
            *self = AudioDevices::NotLoaded;
        }
    }

    pub fn lists(&self) -> Option<&DeviceLists> {
        match self {
            AudioDevices::Ready(lists) => Some(lists),
            _ => None,
        }
    }
}

/// List both audio directions and the MIDI inputs on a fresh thread (MTA COM there only). Blocks the caller, so call it
/// from a background task, never the UI thread.
pub fn enumerate_blocking() -> DeviceLists {
    let worker = std::thread::Builder::new().name("recall-audio-devices".into()).spawn(|| {
        let _ = wasapi::initialize_mta();
        let lists = DeviceLists {
            inputs: names(&Direction::Capture),
            outputs: names(&Direction::Render),
            default_input: default_name(&Direction::Capture),
            default_output: default_name(&Direction::Render),
            mic_inputs: mic_inputs(),
            default_input_is_mic: wasapi::get_default_device(&Direction::Capture).ok().map(|d| is_mic(&d)),
            midi_inputs: crate::services::midi::input_names(),
        };
        wasapi::deinitialize();
        lists
    });
    worker.ok().and_then(|w| w.join().ok()).unwrap_or_default()
}

fn names(direction: &Direction) -> Vec<String> {
    let Ok(collection) = DeviceCollection::new(direction) else { return Vec::new() };
    let n = collection.get_nbr_devices().unwrap_or(0);
    let mut out: Vec<String> = (0..n).filter_map(|i| collection.get_device_at_index(i).ok()).filter_map(|d| d.get_friendlyname().ok()).collect();
    out.sort_by_key(|n| n.to_lowercase());
    out.dedup();
    out
}

fn is_mic(device: &wasapi::Device) -> bool {
    crate::services::endpoint_form::mutes_by_default(device.get_id().ok().and_then(|id| crate::services::endpoint_form::read(&id)))
}

/// Capture endpoints whose form factor mutes by default, by friendly name.
fn mic_inputs() -> Vec<String> {
    let Ok(collection) = DeviceCollection::new(&Direction::Capture) else { return Vec::new() };
    let n = collection.get_nbr_devices().unwrap_or(0);
    (0..n).filter_map(|i| collection.get_device_at_index(i).ok()).filter(is_mic).filter_map(|d| d.get_friendlyname().ok()).collect()
}

fn default_name(direction: &Direction) -> Option<String> {
    wasapi::get_default_device(direction).ok()?.get_friendlyname().ok()
}

/// The row label for a saved device name: "" = the Windows default (named when known); a name
/// that matches no listed endpoint the way the recorder matches it = "<name> (not connected)";
/// a match shows the endpoint's real name (a saved "3- X" that opens "4- X" reads "4- X").
pub fn device_label(saved: &str, listed: Option<&[String]>, default: Option<&str>) -> String {
    let saved = saved.trim();
    if saved.is_empty() {
        return match default {
            Some(name) => format!("Windows default ({name})"),
            None => "Windows default".to_owned(),
        };
    }
    match listed.map(|list| (list, crate::services::meters::match_endpoint(list, saved))) {
        Some((list, Some(i))) => list[i].trim().to_owned(),
        Some((_, None)) => format!("{saved} (not connected)"),
        None => saved.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-07 audit A9: the MIDI picker's list was read once per launch; opening Settings
    /// again reads it afresh.
    #[test]
    fn opening_settings_again_reads_the_devices_afresh() {
        let mut ready = AudioDevices::Ready(DeviceLists { midi_inputs: vec!["Pad".into()], ..DeviceLists::default() });
        ready.stale();
        assert!(matches!(ready, AudioDevices::NotLoaded), "{ready:?}");
        let mut loading = AudioDevices::Loading;
        loading.stale();
        assert!(matches!(loading, AudioDevices::Loading), "a read in flight is kept");
    }

    #[test]
    fn labels_name_the_default_and_missing_devices() {
        let list = vec!["Microphone (USB)".to_owned(), "Microphone (USB Audio Device)".to_owned()];
        assert_eq!(device_label("", Some(&list), Some("Microphone (USB)")), "Windows default (Microphone (USB))");
        assert_eq!(device_label("", None, None), "Windows default");
        assert_eq!(device_label("microphone (usb)", Some(&list), None), "microphone (usb) (not connected)", "case included, like the engine");
        assert_eq!(device_label("Headset (BT)", Some(&list), None), "Headset (BT) (not connected)");
        assert_eq!(device_label("Headset (BT)", None, None), "Headset (BT)", "not listed yet: no claim either way");
        assert_eq!(device_label("usb audio microphone", Some(&list), None), "usb audio microphone (not connected)", "no word matching");
    }

    /// 2026-11-07 merge-gate A4 rule: the page shows the endpoint the recorder opens by its real
    /// name, so a saved "3- X" that opens "4- X" reads "4- X".
    #[test]
    fn a_renumbered_device_shows_its_real_name() {
        let list = vec!["Speakers (Box)".to_owned(), "4- Line (Box One)".to_owned()];
        assert_eq!(device_label("3- Line (Box One)", Some(&list), None), "4- Line (Box One)");
        let two = vec!["4- Line (Box One)".to_owned(), "5- Line (Box One)".to_owned()];
        assert_eq!(device_label("3- Line (Box One)", Some(&two), None), "3- Line (Box One) (not connected)", "two matches: waiting");
    }
}
