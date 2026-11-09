//! Read the primary display's native color primaries from its EDID.
//!
//! novideo_srgb builds its sRGB clamp from the panel's *actual* primaries (so the
//! clamp is correct for this specific monitor, not a generic assumption). It gets
//! them from EDID: `HKLM\SYSTEM\CurrentControlSet\Enum\DISPLAY\<vendor+prod>\<inst>\
//! Device Parameters\EDID`, then parses the chromaticity bytes.
//!
//! We locate the primary monitor's device-instance path via `EnumDisplayDevices`
//! (adapter -> attached monitor), walk the registry subkeys under that monitor's
//! hardware ID, and parse the first EDID we find. This avoids a heavyweight EDID
//! crate: the chromaticity block is a fixed 10-byte field at offset 25.
//!
//! If anything here fails we return `Err` and the caller falls back to
//! "reset-only" (no `Accurate` clamp) rather than clamping with wrong primaries.

use std::os::windows::ffi::OsStringExt;

use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP,
    DISPLAY_DEVICE_PRIMARY_DEVICE,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_READ, RRF_RT_REG_BINARY,
};

/// Native panel primaries parsed from EDID, as CIE xy pairs.
#[derive(Debug, Clone, Copy)]
pub struct EdidPrimaries {
    pub red: (f64, f64),
    pub green: (f64, f64),
    pub blue: (f64, f64),
    pub white: (f64, f64),
}

/// Decode a NUL-terminated wide buffer into a String.
fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    std::ffi::OsString::from_wide(&buf[..end])
        .to_string_lossy()
        .into_owned()
}

/// Find the primary monitor's device-instance registry path fragment, e.g.
/// `ABC1234\5&1a2b3c4&0&UID1234`. Returned as (hardware_id, instance_id).
fn primary_monitor_instance() -> Result<(String, String), String> {
    // Pull the monitor device-instance id for an adapter by name. Returns the
    // (hardware_id, instance_id) split from the monitor's DeviceID, if present.
    unsafe fn monitor_id_for_adapter(dev_name: &str) -> Option<(String, String)> {
        let mut monitor = DISPLAY_DEVICEW::default();
        monitor.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
        let name_w: Vec<u16> = dev_name.encode_utf16().chain(std::iter::once(0)).collect();
        // EDID_DEVICE_INTERFACE (flag 1) makes DeviceID carry the device-instance
        // path (\\?\DISPLAY#HWID#INST#{guid}) rather than a monitor friendly name.
        if !EnumDisplayDevicesW(PCWSTR(name_w.as_ptr()), 0, &mut monitor, 1).as_bool() {
            return None;
        }
        // monitor.DeviceID: \\?\DISPLAY#<hwid>#<instance>#{guid}
        let dev_id = wide_to_string(&monitor.DeviceID);
        let parts: Vec<&str> = dev_id.split('#').collect();
        if parts.len() >= 3 && !parts[1].is_empty() && !parts[2].is_empty() {
            Some((parts[1].to_string(), parts[2].to_string()))
        } else {
            None
        }
    }

    unsafe {
        // Two-pass: prefer the adapter flagged PRIMARY; if none matches (some
        // driver/flag combos don't set it as expected), fall back to the first
        // attached adapter that yields a monitor instance. Either way we get the
        // active panel's EDID key.
        let mut fallback: Option<(String, String)> = None;
        let mut adapter_idx = 0u32;
        loop {
            let mut adapter = DISPLAY_DEVICEW::default();
            adapter.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            if !EnumDisplayDevicesW(PCWSTR::null(), adapter_idx, &mut adapter, 0).as_bool() {
                break;
            }
            adapter_idx += 1;

            let flags = adapter.StateFlags.0; // DISPLAY_DEVICE_STATE_FLAGS newtype
            let is_primary = (flags & DISPLAY_DEVICE_PRIMARY_DEVICE.0) != 0;
            let attached = (flags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP.0) != 0;
            if !attached {
                continue;
            }
            let dev_name = wide_to_string(&adapter.DeviceName);

            if let Some(id) = monitor_id_for_adapter(&dev_name) {
                if is_primary {
                    return Ok(id); // best match -> done
                }
                fallback.get_or_insert(id);
            }
        }
        fallback.ok_or_else(|| "no attached monitor found via EnumDisplayDevices".into())
    }
}

/// Open a registry subkey (read-only) under a parent HKEY.
unsafe fn open_subkey(parent: HKEY, sub: &str) -> Result<HKEY, String> {
    let sub_w: Vec<u16> = sub.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key = HKEY::default();
    let st = RegOpenKeyExW(parent, PCWSTR(sub_w.as_ptr()), Some(0), KEY_READ, &mut key);
    if st.is_ok() {
        Ok(key)
    } else {
        Err(format!("RegOpenKeyExW({sub}) failed: {st:?}"))
    }
}

/// Read the `EDID` binary value from a `...\Device Parameters` key.
unsafe fn read_edid_value(key: HKEY) -> Result<Vec<u8>, String> {
    let sub_w: Vec<u16> = "Device Parameters"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let name_w: Vec<u16> = "EDID".encode_utf16().chain(std::iter::once(0)).collect();

    let mut size: u32 = 0;
    // First call to size the buffer.
    let st = RegGetValueW(
        key,
        PCWSTR(sub_w.as_ptr()),
        PCWSTR(name_w.as_ptr()),
        RRF_RT_REG_BINARY,
        None,
        None,
        Some(&mut size),
    );
    if st.is_err() || size == 0 {
        return Err(format!("size EDID value failed: {st:?}"));
    }
    let mut buf = vec![0u8; size as usize];
    let st = RegGetValueW(
        key,
        PCWSTR(sub_w.as_ptr()),
        PCWSTR(name_w.as_ptr()),
        RRF_RT_REG_BINARY,
        None,
        Some(buf.as_mut_ptr() as *mut _),
        Some(&mut size),
    );
    if st.is_err() {
        return Err(format!("read EDID value failed: {st:?}"));
    }
    buf.truncate(size as usize);
    Ok(buf)
}

/// Locate + read the EDID bytes for the primary monitor.
fn read_primary_edid() -> Result<Vec<u8>, String> {
    let (hw_id, inst_id) = primary_monitor_instance()?;
    let base = format!("SYSTEM\\CurrentControlSet\\Enum\\DISPLAY\\{hw_id}");

    unsafe {
        let hw_key = open_subkey(HKEY_LOCAL_MACHINE, &base)?;

        // The instance subkey name matches inst_id, but registry stores it with
        // `&` intact; enumerate children and match by prefix to be robust.
        let mut idx = 0u32;
        let mut chosen: Option<HKEY> = None;
        loop {
            let mut name_buf = [0u16; 256];
            let mut name_len = name_buf.len() as u32;
            let st = RegEnumKeyExW(
                hw_key,
                idx,
                Some(windows::core::PWSTR(name_buf.as_mut_ptr())),
                &mut name_len,
                None,
                None,
                None,
                None,
            );
            if st.is_err() {
                break;
            }
            idx += 1;
            let child = wide_to_string(&name_buf[..name_len as usize]);
            if child == inst_id || child.starts_with(inst_id.split('&').next().unwrap_or(&inst_id)) {
                // Open the *instance* key; RegGetValueW then reaches into its
                // `Device Parameters` subkey for the EDID value.
                if let Ok(k) = open_subkey(hw_key, &child) {
                    chosen = Some(k);
                    break;
                }
            }
        }
        let _ = RegCloseKey(hw_key);

        let inst_key = chosen.ok_or_else(|| {
            format!("no matching instance subkey under {base} for {inst_id}")
        })?;
        let edid = read_edid_value(inst_key);
        let _ = RegCloseKey(inst_key);
        edid
    }
}

/// Parse the CIE chromaticity block from a raw 128+ byte EDID.
///
/// EDID 1.x packs R/G/B/W xy as 10 bits each across bytes 25..=34:
/// - byte 25: Rx1 Rx0 Ry1 Ry0 Gx1 Gx0 Gy1 Gy0  (high 2 bits of each)
/// - byte 26: Bx1 Bx0 By1 By0 Wx1 Wx0 Wy1 Wy0
/// - bytes 27..34: the high 8 bits of Rx,Ry,Gx,Gy,Bx,By,Wx,Wy respectively.
/// Value = (high8 << 2 | low2) / 1024.
fn parse_chromaticity(edid: &[u8]) -> Result<EdidPrimaries, String> {
    if edid.len() < 35 {
        return Err(format!("EDID too short: {} bytes", edid.len()));
    }
    // Basic header sanity: 00 FF FF FF FF FF FF 00.
    if &edid[0..8] != &[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return Err("EDID header signature mismatch".into());
    }

    let b25 = edid[25];
    let b26 = edid[26];

    // Reconstruct each 10-bit value: high 8 bits from bytes 27..=34, low 2 bits
    // packed 2-per-nibble in bytes 25/26.
    let val = |high8: u8, low2: u8| -> f64 { (((high8 as u16) << 2) | (low2 as u16)) as f64 / 1024.0 };

    let rx_lo = (b25 >> 6) & 0x3;
    let ry_lo = (b25 >> 4) & 0x3;
    let gx_lo = (b25 >> 2) & 0x3;
    let gy_lo = b25 & 0x3;
    let bx_lo = (b26 >> 6) & 0x3;
    let by_lo = (b26 >> 4) & 0x3;
    let wx_lo = (b26 >> 2) & 0x3;
    let wy_lo = b26 & 0x3;

    let rx = val(edid[27], rx_lo);
    let ry = val(edid[28], ry_lo);
    let gx = val(edid[29], gx_lo);
    let gy = val(edid[30], gy_lo);
    let bx = val(edid[31], bx_lo);
    let by = val(edid[32], by_lo);
    let wx = val(edid[33], wx_lo);
    let wy = val(edid[34], wy_lo);

    // Reject clearly-bogus parses (zero or out-of-range).
    for v in [rx, ry, gx, gy, bx, by, wx, wy] {
        if !(0.0..=1.0).contains(&v) || v == 0.0 {
            return Err("EDID chromaticity out of range".into());
        }
    }

    Ok(EdidPrimaries {
        red: (rx, ry),
        green: (gx, gy),
        blue: (bx, by),
        white: (wx, wy),
    })
}

/// Read + parse the primary display's native primaries from EDID.
pub fn primary_display_primaries() -> Result<EdidPrimaries, String> {
    let edid = read_primary_edid()?;
    parse_chromaticity(&edid)
}
