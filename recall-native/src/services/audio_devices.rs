//! Audio endpoints for Settings → Clip → Audio (Microphone, Desktop audio) and MIDI inputs for
//! Settings → General. Listed lazily: the first time either tab opens, on a thread of its own (COM lives and dies there), once per
//! launch. Nothing here runs before first paint.
use wasapi::{DeviceCollection, Direction};

/// Active endpoints by friendly name, as Windows shows them (and as the engine matches them).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceLists {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub default_input: Option<String>,
    pub default_output: Option<String>,
    /// MIDI input ports (Settings → General → MIDI device).
    pub midi_inputs: Vec<String>,
}

/// Load state; the list is fetched once and kept for the launch.
#[derive(Debug, Default)]
pub enum AudioDevices {
    #[default]
    NotLoaded,
    Loading,
    Ready(DeviceLists),
}

impl AudioDevices {
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

fn default_name(direction: &Direction) -> Option<String> {
    wasapi::get_default_device(direction).ok()?.get_friendlyname().ok()
}

/// The row label for a saved device name: "" = the Windows default (named when known); a name
/// Windows no longer lists = "<name> (not connected)".
pub fn device_label(saved: &str, listed: Option<&[String]>, default: Option<&str>) -> String {
    let saved = saved.trim();
    if saved.is_empty() {
        return match default {
            Some(name) => format!("Windows default ({name})"),
            None => "Windows default".to_owned(),
        };
    }
    match listed {
        Some(list) if !list.iter().any(|n| n.trim().eq_ignore_ascii_case(saved)) => format!("{saved} (not connected)"),
        _ => saved.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_name_the_default_and_missing_devices() {
        let list = vec!["Microphone (USB)".to_owned(), "Microphone (3- RODECaster Duo Chat)".to_owned()];
        assert_eq!(device_label("", Some(&list), Some("Microphone (USB)")), "Windows default (Microphone (USB))");
        assert_eq!(device_label("", None, None), "Windows default");
        assert_eq!(device_label("microphone (usb)", Some(&list), None), "microphone (usb)", "case-insensitive like the engine");
        assert_eq!(device_label("Headset (BT)", Some(&list), None), "Headset (BT) (not connected)");
        assert_eq!(device_label("Headset (BT)", None, None), "Headset (BT)", "not listed yet: no claim either way");
    }
}
