//! Whether the primary display can turn HDR on, read from CCD (`QueryDisplayConfig` +
//! `GET_ADVANCED_COLOR_INFO`, bit 0 `advancedColorSupported`). DXGI only says whether HDR is
//! on right now (`hdr::primary_hdr`); the capture format needs to know whether it *can* come
//! on mid-game (`capture::format::start_fp16`). The primary display is the active path whose
//! source sits at the desktop origin, as `hdr::primary_hdr` and the controller pick it.

use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_PATH_INFO, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::ERROR_SUCCESS;

/// `advancedColorSupported` for the primary display; `None` when CCD cannot say.
pub fn primary_hdr_capable() -> Option<bool> {
    let (paths, modes) = active_paths()?;
    let primary = paths.iter().find(|p| {
        // SAFETY: without QDC_VIRTUAL_MODE_AWARE the union holds `modeInfoIdx`.
        let idx = unsafe { p.sourceInfo.Anonymous.modeInfoIdx } as usize;
        modes.get(idx).is_some_and(|m| {
            // SAFETY: a SOURCE mode's union holds `sourceMode`.
            m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE && unsafe { m.Anonymous.sourceMode.position.x == 0 && m.Anonymous.sourceMode.position.y == 0 }
        })
    })?;
    let mut req = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            size: std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
            adapterId: primary.targetInfo.adapterId,
            id: primary.targetInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: `req` is a correctly sized GET_ADVANCED_COLOR_INFO packet.
    let st = unsafe { DisplayConfigGetDeviceInfo(&mut req.header) };
    if st != ERROR_SUCCESS.0 as i32 {
        return None;
    }
    // SAFETY: the bitfield view of the union is always valid to read.
    Some(unsafe { req.Anonymous.Anonymous._bitfield } & 0b1 != 0)
}

/// Active paths and the mode table their indices point into.
fn active_paths() -> Option<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
    let (mut n_paths, mut n_modes) = (0u32, 0u32);
    // SAFETY: plain out-parameters.
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes) } != ERROR_SUCCESS {
        return None;
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
    // SAFETY: the buffers hold the sizes passed in.
    let st = unsafe { QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, paths.as_mut_ptr(), &mut n_modes, modes.as_mut_ptr(), None) };
    if st != ERROR_SUCCESS {
        return None;
    }
    paths.truncate(n_paths as usize);
    modes.truncate(n_modes as usize);
    Some((paths, modes))
}
