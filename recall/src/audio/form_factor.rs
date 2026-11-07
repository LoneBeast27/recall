//! Whether an input follows the mic-mute hotkey when its source does not say
//! (`SourceSpec::mic_mute` absent): from the endpoint's Windows form factor
//! (`PKEY_AudioEndpoint_FormFactor`), never from its name (SPEC A12). A microphone, headset or
//! handset mutes; a line-level input, S/PDIF, an HDMI/DisplayPort capture or an unknown form
//! factor keeps its sound. The Audio page keeps a copy (`recall-native` `services/endpoint_form.rs`).

/// `EndpointFormFactor` values (mmdeviceapi.h).
const MICROPHONE: u32 = 4;
const HEADSET: u32 = 5;
const HANDSET: u32 = 6;

/// The default for an endpoint whose form factor reads `form`. `None` (the property could not be
/// read) mutes: a device that may be a microphone is never left live by a failed read.
pub(crate) fn mutes_by_default(form: Option<u32>) -> bool {
    match form {
        None => true,
        Some(f) => matches!(f, MICROPHONE | HEADSET | HANDSET),
    }
}

/// A source's own say (`SourceSpec::mic_mute`) as `TrackRing::mic_pref`: none (the endpoint's
/// form factor decides), on, off.
const MUTE_AUTO: u8 = 0;
const MUTE_ON: u8 = 1;
const MUTE_OFF: u8 = 2;

pub(super) fn pref(mic_mute: Option<bool>) -> u8 {
    match mic_mute {
        None => MUTE_AUTO,
        Some(true) => MUTE_ON,
        Some(false) => MUTE_OFF,
    }
}

/// Whether an input with this `pref` mutes, its endpoint being a microphone or not.
pub(super) fn decide(pref: u8, endpoint_is_mic: bool) -> bool {
    match pref {
        MUTE_ON => true,
        MUTE_OFF => false,
        _ => endpoint_is_mic,
    }
}

/// The form factor of the endpoint `id` (`IMMDevice::GetId`), on a thread with COM initialized.
pub(super) fn read(id: &str) -> Option<u32> {
    use windows::core::PCWSTR;
    use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, PKEY_AudioEndpoint_FormFactor};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL, STGM_READ};
    use windows::Win32::System::Com::StructuredStorage::PropVariantToUInt32;
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: plain COM calls on interfaces this function owns; `wide` outlives `GetDevice`.
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDevice(PCWSTR(wide.as_ptr())).ok()?;
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let value = store.GetValue(&PKEY_AudioEndpoint_FormFactor).ok()?;
        PropVariantToUInt32(&value).ok()
    }
}
