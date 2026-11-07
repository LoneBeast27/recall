//! The endpoint a saved source's device opens, as the Audio page shows it, with the recorder's
//! endpoint-id tie-break (A4 amendment, merge gate 2026-11-07 08:29 UTC; `meters::pick_endpoint`):
//! among two or more endpoints with the source's name, the one with its saved id. Only inputs
//! carry endpoint ids on the page (an output saves none), so an output never tie-breaks.
use crate::services::audio_devices::{found, found_from, label_for, DeviceLists, Found};
use crate::services::audio_sources::Source;
use crate::services::endpoint_choice::choose;

/// [`found`] for `saved` (and the endpoint id saved with it) among `listed`, whose ids are `ids`
/// by index ("" unreadable): the one same-named endpoint with the saved id opens.
pub fn found_with_ids(saved: &str, saved_id: &str, listed: Option<&[String]>, ids: Option<&[String]>) -> Found {
    if let (Some(names), Some(ids), false) = (listed, ids, saved.trim().is_empty()) {
        let id_of = |i: usize| ids.get(i).map(|id| id.trim().to_owned()).filter(|id| !id.is_empty());
        return found_from(choose(names, saved, saved_id, id_of), names, saved);
    }
    found(saved, listed)
}

/// What `source`'s device opens among `lists` (its direction's list; inputs with their ids).
pub fn source_found(source: &Source, lists: Option<&DeviceLists>) -> Found {
    let inputs = source.kind.uses_inputs();
    let names = lists.map(|l| if inputs { l.inputs.as_slice() } else { l.outputs.as_slice() });
    let ids = lists.filter(|_| inputs).map(|l| l.input_ids.as_slice());
    found_with_ids(&source.device, &source.device_id, names, ids)
}

/// The row label of `source`'s device (`audio_devices::device_label`, with the tie-break): an
/// endpoint picked by its id shows its real name.
pub fn source_device_label(source: &Source, lists: Option<&DeviceLists>, default: Option<&str>) -> String {
    let names = lists.map(|l| if source.kind.uses_inputs() { l.inputs.as_slice() } else { l.outputs.as_slice() });
    label_for(&source.device, source_found(source, lists), names, default)
}

#[cfg(test)]
#[path = "audio_pick_tests.rs"]
mod tests;
