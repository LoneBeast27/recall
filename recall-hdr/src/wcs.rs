//! WCS Advanced-Color profile association (the modern mscms path).
//!
//! Sets which ICC profile the Advanced-Color/HDR pipeline treats as the
//! display default — used to switch the SDR-in-HDR MHC2 variant
//! (punchy ⇄ accurate). Mechanism proven live 2026-07-08 via
//! `ColorProfileAddDisplayAssociation` (S_OK + `ColorProfileGetDisplayDefault`
//! confirms); raw-registry writes are NOT consumed by build 26200.
//!
//! The API addresses a display by (adapter LUID, SOURCE id) — the source half
//! of the CCD path, not the target half the rest of this app uses.

use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, LUID};

#[link(name = "mscms")]
extern "system" {
    /// HRESULT ColorProfileAddDisplayAssociation(scope, PCWSTR profile, LUID, DWORD sourceId, BOOL default, BOOL advancedColor)
    fn ColorProfileAddDisplayAssociation(
        scope: i32,
        profile_name: *const u16,
        target_adapter_id: LUID,
        source_id: u32,
        set_as_default: i32,
        associate_as_advanced_color: i32,
    ) -> i32;
}

const SCOPE_CURRENT_USER: i32 = 1;

/// (adapter LUID, source id) for the active path whose TARGET friendly name
/// matches `wanted`.
fn source_for_target_name(wanted: &str) -> Result<(LUID, u32), String> {
    unsafe {
        let mut n_paths: u32 = 0;
        let mut n_modes: u32 = 0;
        let st = GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes);
        if st != ERROR_SUCCESS {
            return Err(format!("GetDisplayConfigBufferSizes failed: {st:?}"));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        let st = QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_paths,
            paths.as_mut_ptr(),
            &mut n_modes,
            modes.as_mut_ptr(),
            None,
        );
        if st != ERROR_SUCCESS {
            return Err(format!("QueryDisplayConfig failed: {st:?}"));
        }
        paths.truncate(n_paths as usize);

        for p in paths.iter().filter(|p| p.targetInfo.targetAvailable.as_bool()) {
            let mut req = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                    adapterId: p.targetInfo.adapterId,
                    id: p.targetInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut req.header) != ERROR_SUCCESS.0 as i32 {
                continue;
            }
            let name = String::from_utf16_lossy(&req.monitorFriendlyDeviceName)
                .trim_end_matches('\0')
                .to_string();
            if name.eq_ignore_ascii_case(wanted) {
                return Ok((p.sourceInfo.adapterId, p.sourceInfo.id));
            }
        }
        Err(format!("no active display named '{wanted}'"))
    }
}

/// Associate `profile_file` (a filename already present in the color store) as
/// the current-user Advanced-Color DEFAULT for the named display. Takes effect
/// on the next HDR entry.
pub fn set_advanced_color_default(display_name: &str, profile_file: &str) -> Result<(), String> {
    let (luid, source_id) = source_for_target_name(display_name)?;
    let wide: Vec<u16> = profile_file.encode_utf16().chain(std::iter::once(0)).collect();
    let hr = unsafe {
        ColorProfileAddDisplayAssociation(SCOPE_CURRENT_USER, wide.as_ptr(), luid, source_id, 1, 1)
    };
    if hr == 0 {
        Ok(())
    } else {
        Err(format!("ColorProfileAddDisplayAssociation hr=0x{hr:08X}"))
    }
}
