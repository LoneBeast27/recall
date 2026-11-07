//! Whether Windows calls an input endpoint a microphone (`PKEY_AudioEndpoint_FormFactor`). Only
//! a hint in the strip menu ("Windows reports this as a line-level input"): Alt+M follows the
//! strip's own flag and mutes an input without one (R3-deep-audio-1), never by form factor.

/// `EndpointFormFactor` values (mmdeviceapi.h).
const MICROPHONE: u32 = 4;
const HEADSET: u32 = 5;
const HANDSET: u32 = 6;

/// A microphone, headset or handset is a mic; a line-level input, S/PDIF, an HDMI/DisplayPort
/// capture or an unknown form factor is not a microphone. `None` (unreadable) counts as one.
pub fn mutes_by_default(form: Option<u32>) -> bool {
    match form {
        None => true,
        Some(f) => matches!(f, MICROPHONE | HEADSET | HANDSET),
    }
}

/// The form factor of the endpoint `id` (`IMMDevice::GetId`), on a thread with COM initialized.
pub fn read(id: &str) -> Option<u32> {
    use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, PKEY_AudioEndpoint_FormFactor};
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, STGM_READ};
    use windows::Win32::System::Com::StructuredStorage::PropVariantToUInt32;
    use windows::core::PCWSTR;
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

#[cfg(test)]
mod tests {
    use super::mutes_by_default;

    #[test]
    fn only_microphone_form_factors_mute_by_default() {
        for (form, mutes) in [(Some(4), true), (Some(5), true), (Some(6), true), (Some(2), false), (Some(8), false), (Some(9), false), (Some(10), false), (None, true)] {
            assert_eq!(mutes_by_default(form), mutes, "form factor {form:?}");
        }
    }
}
