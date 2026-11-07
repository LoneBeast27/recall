//! Whether Windows calls an input endpoint line-level (`PKEY_AudioEndpoint_FormFactor`). Only a
//! hint in the strip menu ("Windows reports this as a line-level input"): Alt+M follows the
//! strip's own flag and mutes an input without one (R3-deep-audio-1), never by form factor.

/// `EndpointFormFactor::LineLevel` (mmdeviceapi.h).
const LINE_LEVEL: u32 = 2;

/// Only a line-level form factor: unknown (many virtual mics), S/PDIF, HDMI/DisplayPort and an
/// unreadable one are not called line-level (R4-deep-audio-2).
pub fn is_line_level(form: Option<u32>) -> bool {
    form == Some(LINE_LEVEL)
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
    use super::is_line_level;

    /// 2026-11-07 audit R4-deep-audio-2: only a line-level form factor (2) is called line-level;
    /// unknown (10), S/PDIF (8), HDMI/DisplayPort (9) and unreadable are not.
    #[test]
    fn only_a_line_level_form_factor_is_called_line_level() {
        for (form, line) in [(Some(2), true), (Some(10), false), (Some(8), false), (Some(9), false), (Some(4), false), (None, false)] {
            assert_eq!(is_line_level(form), line, "form factor {form:?}");
        }
    }
}
