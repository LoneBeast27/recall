//! Audio endpoints for Settings → Clip → Audio (Microphone, Desktop audio) and MIDI inputs for
//! Settings → General. Listed lazily: the first time either tab opens, on a thread of its own (COM lives and dies there), and
//! again each time Settings opens (`stale`). Nothing here runs before first paint.
use wasapi::{DeviceCollection, Direction};

#[path = "../../../recall/src/communications_audio.rs"]
pub(crate) mod communications_audio;

/// Active endpoints by friendly name, as Windows shows them (and as the engine matches them).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceLists {
    pub inputs: Vec<String>,
    /// Each input's endpoint id (`IMMDevice::GetId`), by index of `inputs` ("" unreadable):
    /// saved with a picked device, so a line-in skips Alt+M only on that endpoint.
    pub input_ids: Vec<String>,
    /// Each input's Windows form factor, by index of `inputs` (`None` unreadable). A hint only.
    pub input_forms: Vec<Option<u32>>,
    pub outputs: Vec<String>,
    pub default_input: Option<String>,
    pub default_output: Option<String>,
    pub communications_output: Option<String>,
    /// The inputs whose Windows form factor is line-level (`endpoint_form`). Only a hint in the
    /// strip menu: Alt+M follows the strip's flag.
    pub line_inputs: Vec<String>,
    /// The default input is line-level (`None`: there is none).
    pub default_input_is_line: Option<bool>,
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
        let (inputs, input_ids, input_forms) = inputs();
        let line_inputs = inputs.iter().zip(&input_forms).filter(|(_, form)| crate::services::endpoint_form::is_line_level(**form)).map(|(n, _)| n.clone()).collect();
        let lists = DeviceLists {
            inputs,
            input_ids,
            input_forms,
            outputs: names(&Direction::Render),
            default_input: default_name(&Direction::Capture),
            default_output: default_name(&Direction::Render),
            communications_output: communications_audio::distinct_device().and_then(|d| d.get_friendlyname().ok()),
            line_inputs,
            default_input_is_line: wasapi::get_default_device(&Direction::Capture).ok().map(|d| is_line(&d)),
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
    sorted((0..n).filter_map(|i| collection.get_device_at_index(i).ok()).filter_map(|d| d.get_friendlyname().ok()).collect())
}

/// Sorted by name, twins kept: the engine and the meter see every endpoint, so two with the same
/// name both show (R4-deep-audio-3) and the page never calls a twin connected.
fn sorted(mut out: Vec<String>) -> Vec<String> {
    out.sort_by_key(|n| n.to_lowercase());
    out
}

/// How many listed endpoints carry exactly `name`.
fn twins(names: &[String], name: &str) -> usize {
    names.iter().filter(|n| n.trim() == name.trim()).count()
}

/// The device picker's row labels: a name two or more endpoints share is marked "1 of 2" etc.,
/// so the twins read apart (Windows gives them no other name). The recorder opens neither while
/// both are there; renaming one in Windows fixes it.
pub fn picker_labels(names: &[String]) -> Vec<String> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| match twins(names, name) {
            0 | 1 => name.clone(),
            n => format!("{name} ({} of {n} with this name)", twins(&names[..i], name) + 1),
        })
        .collect()
}

fn is_line(device: &wasapi::Device) -> bool {
    crate::services::endpoint_form::is_line_level(device.get_id().ok().and_then(|id| crate::services::endpoint_form::read(&id)))
}

/// Capture endpoints: names sorted like `sorted`, with each one's id and form factor.
fn inputs() -> (Vec<String>, Vec<String>, Vec<Option<u32>>) {
    let Ok(collection) = DeviceCollection::new(&Direction::Capture) else { return Default::default() };
    let n = collection.get_nbr_devices().unwrap_or(0);
    let mut out: Vec<(String, String, Option<u32>)> = (0..n)
        .filter_map(|i| collection.get_device_at_index(i).ok())
        .filter_map(|d| {
            let id = d.get_id().unwrap_or_default();
            let form = Some(id.as_str()).filter(|id| !id.is_empty()).and_then(crate::services::endpoint_form::read);
            Some((d.get_friendlyname().ok()?, id, form))
        })
        .collect();
    out.sort_by_key(|(n, _, _)| n.to_lowercase());
    let forms = out.iter().map(|(_, _, f)| *f).collect();
    let ids = out.iter().map(|(_, id, _)| id.clone()).collect();
    (out.into_iter().map(|(n, _, _)| n).collect(), ids, forms)
}

fn default_name(direction: &Direction) -> Option<String> {
    wasapi::get_default_device(direction).ok()?.get_friendlyname().ok()
}

/// What the recorder does with a saved device name, from the listed endpoints (`match_endpoint`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// No device: the Windows default.
    Default,
    /// The lists are not read yet: no claim either way.
    Unknown,
    /// It opens the endpoint at this index.
    Opens(usize),
    /// `n` endpoints carry exactly this name: it opens none.
    Twins(usize),
    /// `n` endpoints match once a leading "<digits>- " index is dropped: it opens none.
    Ambiguous(usize),
    /// `twins` endpoints share the name, none by the saved id; this one has exactly the saved
    /// name. An input records it muted until a fresh Alt+M; an output waits (A4 ruling).
    Unsure { index: usize, twins: usize },
    /// Only a name in another case is listed: it waits (P2-d).
    CaseOnly,
    /// No endpoint matches.
    Missing,
}

/// [`Found`] for `saved` among `listed`.
pub fn found(saved: &str, listed: Option<&[String]>) -> Found {
    let saved = saved.trim();
    if saved.is_empty() {
        return Found::Default;
    }
    let Some(list) = listed else { return Found::Unknown };
    found_from(crate::services::endpoint_choice::choose(list, saved, "", |_| None), list, saved)
}

/// [`Found`] for the recorder's pick `choice` of `saved` among `list`.
pub fn found_from(choice: crate::services::endpoint_choice::Choice, list: &[String], saved: &str) -> Found {
    use crate::services::endpoint_choice::Choice;
    match choice {
        Choice::Opens(i) => Found::Opens(i),
        Choice::Unsure { index, twins } => Found::Unsure { index, twins },
        Choice::Twins(_) if twins(list, saved) > 1 => Found::Twins(twins(list, saved)),
        Choice::Twins(n) => Found::Ambiguous(n),
        Choice::CaseOnly => Found::CaseOnly,
        Choice::Missing => Found::Missing,
    }
}

/// The row label for a saved device name: "" = the Windows default (named when known); a name
/// that matches no listed endpoint the way the recorder matches it = "<name> (not connected)";
/// a match shows the endpoint's real name (a saved "3- X" that opens "4- X" reads "4- X"); a
/// name two or more endpoints share or match says how many (R5-deep-audio-4).
pub fn device_label(saved: &str, listed: Option<&[String]>, default: Option<&str>) -> String {
    label_for(saved, found(saved, listed), listed, default)
}

/// [`device_label`] for a [`Found`] already worked out (`audio_pick::source_found` adds the
/// endpoint-id tie-break).
pub fn label_for(saved: &str, found: Found, listed: Option<&[String]>, default: Option<&str>) -> String {
    let saved = saved.trim();
    match found {
        Found::Default => match default {
            Some(name) => format!("Windows default ({name})"),
            None => "Windows default".to_owned(),
        },
        Found::Opens(i) | Found::Unsure { index: i, .. } => listed.map_or(saved, |l| l[i].trim()).to_owned(),
        Found::CaseOnly => format!("{saved} (not found exactly; pick it again)"),
        Found::Twins(n) => format!("{saved} ({n} devices have this name; rename one in Windows)"),
        Found::Ambiguous(n) => format!("{saved} ({n} devices match; rename one in Windows)"),
        Found::Missing => format!("{saved} (not connected)"),
        Found::Unknown => saved.to_owned(),
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
        assert_eq!(device_label("microphone (usb)", Some(&list), None), "microphone (usb) (not found exactly; pick it again)", "case included, like the engine (P2-d)");
        assert_eq!(device_label("Headset (BT)", Some(&list), None), "Headset (BT) (not connected)");
        assert_eq!(device_label("Headset (BT)", None, None), "Headset (BT)", "not listed yet: no claim either way");
        assert_eq!(device_label("usb audio microphone", Some(&list), None), "usb audio microphone (not connected)", "no word matching");
    }

    /// 2026-11-07 audit R4-deep-audio-3: two endpoints with the same name are both listed (as the
    /// engine and the meter see them), each marked in the picker, and a saved twin says why it
    /// does not record instead of reading as connected.
    #[test]
    fn twin_devices_are_both_listed_and_marked() {
        let twin = "Line (Box One)".to_owned();
        let listed = sorted(vec![twin.clone(), "Speakers (Box)".into(), twin.clone()]);
        assert_eq!(listed, [twin.clone(), twin.clone(), "Speakers (Box)".to_owned()], "both twins kept");
        let label = device_label(&twin, Some(&listed), None);
        assert!(label.starts_with("Line (Box One) (") && label.contains("2 devices"), "{label}");
        let labels = picker_labels(&listed);
        assert_eq!(labels.len(), 3);
        assert!(labels[0] != twin && labels[1] != twin && labels[0] != labels[1], "each twin is marked: {labels:?}");
        assert_eq!(labels[2], "Speakers (Box)", "a unique name is unchanged");
    }

    /// 2026-11-07 merge-gate A4 rule: the page shows the endpoint the recorder opens by its real
    /// name, so a saved "3- X" that opens "4- X" reads "4- X".
    #[test]
    fn a_renumbered_device_shows_its_real_name() {
        let list = vec!["Speakers (Box)".to_owned(), "4- Line (Box One)".to_owned()];
        assert_eq!(device_label("3- Line (Box One)", Some(&list), None), "4- Line (Box One)");
        let two = vec!["4- Line (Box One)".to_owned(), "5- Line (Box One)".to_owned()];
        assert!(!device_label("3- Line (Box One)", Some(&two), None).ends_with("(not connected)"), "two matches: connected, but ambiguous");
    }

    /// 2026-11-07 audit R5-deep-audio-4: a saved "3- X" with "4- X" and "5- X" both there reads
    /// as ambiguous (both are connected; neither is recorded), and twins say how many there are.
    #[test]
    fn ambiguous_and_twin_names_count_the_devices() {
        let two = vec!["4- Line (Box One)".to_owned(), "5- Line (Box One)".to_owned()];
        assert_eq!(device_label("3- Line (Box One)", Some(&two), None), "3- Line (Box One) (2 devices match; rename one in Windows)");
        assert_eq!(found("3- Line (Box One)", Some(&two)), Found::Ambiguous(2));
        let three = vec!["Line (Box One)".to_owned(); 3];
        assert_eq!(found("Line (Box One)", Some(&three)), Found::Twins(3));
        assert!(device_label("Line (Box One)", Some(&three), None).contains("3 devices"));
        assert_eq!(found("Line (Box One)", None), Found::Unknown);
        assert_eq!(found(" ", Some(&three)), Found::Default);
    }
}
