//! Volume → physical drive classification (SSD / HDD / drive-managed SMR HDD).
//!
//! Per volume, once: `IOCTL_STORAGE_QUERY_PROPERTY` for `StorageDeviceSeekPenaltyProperty`
//! (does the device pay a seek penalty) and `StorageDeviceProperty` (vendor / product /
//! bus), plus `IOCTL_STORAGE_GET_DEVICE_NUMBER` (which physical disk, so two volumes on one
//! disk share one I/O lane). The device is opened with zero access rights, which these
//! queries allow without elevation.
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW};
use windows::Win32::System::IO::DeviceIoControl;
use windows::core::PCWSTR;

use super::smr;

const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
const STORAGE_DEVICE_PROPERTY: u32 = 0;
const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: u32 = 7;
const PROPERTY_STANDARD_QUERY: u32 = 0;
/// `STORAGE_BUS_TYPE::BusTypeNvme`.
pub(super) const BUS_NVME: u32 = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriveKind {
    Ssd,
    Hdd,
    /// Drive-managed shingled HDD: reads like an HDD, small random writes are very costly.
    SmrHdd,
}

impl DriveKind {
    /// "SSD", "HDD", "SMR HDD".
    pub fn label(self) -> &'static str {
        match self {
            DriveKind::Ssd => "SSD",
            DriveKind::Hdd => "HDD",
            DriveKind::SmrHdd => "SMR HDD",
        }
    }

    pub fn is_hdd(self) -> bool {
        self != DriveKind::Ssd
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DriveInfo {
    /// Volume root, e.g. `E:\`.
    pub volume: String,
    /// Device model, e.g. "ST4000DM004-2CV104"; empty when the device did not report one.
    pub model: String,
    pub kind: DriveKind,
    pub free: u64,
    pub total: u64,
    /// Physical disk number (`\\.\PhysicalDriveN`) when the volume sits on one disk.
    pub disk: Option<u32>,
}

#[derive(Debug, Clone)]
struct Device {
    model: String,
    kind: DriveKind,
    disk: Option<u32>,
}

/// Parsed `STORAGE_DEVICE_DESCRIPTOR` fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Descriptor {
    pub vendor: String,
    pub product: String,
    pub bus: u32,
}

impl Descriptor {
    pub fn model(&self) -> String {
        let vendor = self.vendor.trim();
        let product = self.product.trim();
        if vendor.is_empty() || product.to_ascii_uppercase().starts_with(&vendor.to_ascii_uppercase()) {
            product.to_owned()
        } else {
            format!("{vendor} {product}")
        }
    }
}

/// Pure classification. A known SMR model wins; otherwise the seek-penalty answer decides;
/// with no answer, NVMe is an SSD and anything else is treated as an HDD (the safe side:
/// it only costs serialized reads).
pub(super) fn classify(seek_penalty: Option<bool>, model: &str, bus: u32) -> DriveKind {
    if smr::is_drive_managed_smr(model) {
        return DriveKind::SmrHdd;
    }
    match seek_penalty {
        Some(false) => DriveKind::Ssd,
        Some(true) => DriveKind::Hdd,
        None if bus == BUS_NVME => DriveKind::Ssd,
        None => DriveKind::Hdd,
    }
}

/// Reads vendor/product strings (NUL-terminated at their offsets; 0 = absent) and the bus.
pub(super) fn parse_descriptor(buffer: &[u8]) -> Option<Descriptor> {
    let u32_at = |offset: usize| buffer.get(offset..offset + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let size = u32_at(4)? as usize;
    let valid = &buffer[..size.min(buffer.len())];
    let text = |offset: u32| -> String {
        let offset = offset as usize;
        if offset == 0 || offset >= valid.len() {
            return String::new();
        }
        let end = valid[offset..].iter().position(|b| *b == 0).map_or(valid.len(), |p| offset + p);
        String::from_utf8_lossy(&valid[offset..end]).trim().to_owned()
    };
    Some(Descriptor { vendor: text(u32_at(12)?), product: text(u32_at(16)?), bus: u32_at(28)? })
}

/// Volume root for `path` (`E:\`), uppercase. Drive-letter paths need no system call.
pub(super) fn volume_root(path: &Path) -> Option<String> {
    let plain = super::plain_path(path);
    let text = plain.to_string_lossy();
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Some(format!("{}:\\", (bytes[0] as char).to_ascii_uppercase()));
    }
    let wide: Vec<u16> = plain.as_os_str().encode_wide().chain([0]).collect();
    let mut out = [0u16; 1024];
    unsafe { GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut out) }.ok()?;
    let end = out.iter().position(|c| *c == 0).unwrap_or(out.len());
    Some(String::from_utf16_lossy(&out[..end]).to_uppercase())
}

fn devices() -> &'static Mutex<HashMap<String, Device>> {
    static DEVICES: OnceLock<Mutex<HashMap<String, Device>>> = OnceLock::new();
    DEVICES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn device(volume: &str) -> Device {
    if let Some(found) = devices().lock().ok().and_then(|map| map.get(volume).cloned()) {
        return found;
    }
    let found = query_device(volume).unwrap_or(Device { model: String::new(), kind: DriveKind::Hdd, disk: None });
    if let Ok(mut map) = devices().lock() {
        map.insert(volume.to_owned(), found.clone());
    }
    found
}

/// Drive type, model, physical disk and space of the volume holding `path` (cached per
/// volume except free/total). `None` when the path has no resolvable volume.
pub fn drive_info(path: &Path) -> Option<DriveInfo> {
    let volume = volume_root(path)?;
    let device = device(&volume);
    let (free, total) = space(&volume).unwrap_or((0, 0));
    // Checks only: `RECALL_FAKE_FREE_BYTES=<n>` makes every drive report n bytes free (a low-disk fixture).
    let free = std::env::var("RECALL_FAKE_FREE_BYTES").ok().and_then(|v| v.trim().parse::<u64>().ok()).map_or(free, |fake| fake.min(free));
    Some(DriveInfo { volume, model: device.model, kind: device.kind, free, total, disk: device.disk })
}

/// Cached kind of the drive holding `path`; unknown volumes count as HDD.
pub fn drive_kind(path: &Path) -> DriveKind {
    volume_root(path).map_or(DriveKind::Hdd, |volume| device(&volume).kind)
}

/// (physical disk number, kind) for lane selection.
pub(super) fn disk_of(path: &Path) -> (Option<u32>, Option<String>, DriveKind) {
    match volume_root(path) {
        Some(volume) => {
            let device = device(&volume);
            (device.disk, Some(volume), device.kind)
        }
        None => (None, None, DriveKind::Hdd),
    }
}

fn space(volume: &str) -> Option<(u64, u64)> {
    let wide: Vec<u16> = volume.encode_utf16().chain([0]).collect();
    let (mut free, mut total) = (0u64, 0u64);
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free as *mut u64), Some(&mut total as *mut u64), None) }.ok()?;
    Some((free, total))
}

/// `E:\` → `\\.\E:`; a mount-point volume → `\\?\Volume{GUID}` (no trailing slash).
fn device_path(volume: &str) -> Option<Vec<u16>> {
    let bytes = volume.as_bytes();
    if bytes.len() == 3 && bytes[1] == b':' {
        return Some(format!(r"\\.\{}:", bytes[0] as char).encode_utf16().chain([0]).collect());
    }
    let wide: Vec<u16> = volume.encode_utf16().chain([0]).collect();
    let mut name = [0u16; 64];
    unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(wide.as_ptr()), &mut name) }.ok()?;
    let mut end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    if end > 0 && name[end - 1] == u16::from(b'\\') {
        end -= 1;
    }
    Some(name[..end].iter().copied().chain([0]).collect())
}

fn query_device(volume: &str) -> Option<Device> {
    let path = device_path(volume)?;
    let path = OsString::from_wide(&path[..path.len().saturating_sub(1)]);
    // Zero access rights: enough for these storage queries, no elevation needed.
    let handle = std::fs::OpenOptions::new().access_mode(0).share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0).open(path).ok()?;
    let mut descriptor = vec![0u8; 1024];
    let parsed = property(&handle, STORAGE_DEVICE_PROPERTY, &mut descriptor).and_then(|n| parse_descriptor(&descriptor[..n]));
    let mut seek = [0u8; 12];
    let seek_penalty = property(&handle, STORAGE_DEVICE_SEEK_PENALTY_PROPERTY, &mut seek).filter(|n| *n >= 9).map(|_| seek[8] != 0);
    let mut number = [0u8; 12];
    let disk = ioctl(&handle, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut number)
        .filter(|n| *n >= 8)
        .map(|_| u32::from_le_bytes([number[4], number[5], number[6], number[7]]));
    let (model, bus) = parsed.map(|d| (d.model(), d.bus)).unwrap_or_default();
    Some(Device { kind: classify(seek_penalty, &model, bus), model, disk })
}

fn property(handle: &File, property: u32, out: &mut [u8]) -> Option<usize> {
    let mut query = [0u8; 12];
    query[0..4].copy_from_slice(&property.to_le_bytes());
    query[4..8].copy_from_slice(&PROPERTY_STANDARD_QUERY.to_le_bytes());
    ioctl(handle, IOCTL_STORAGE_QUERY_PROPERTY, &query, out)
}

fn ioctl(handle: &File, code: u32, input: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut returned = 0u32;
    let input_ptr = (!input.is_empty()).then(|| input.as_ptr().cast());
    unsafe { DeviceIoControl(HANDLE(handle.as_raw_handle()), code, input_ptr, input.len() as u32, Some(out.as_mut_ptr().cast()), out.len() as u32, Some(&mut returned), None) }
        .ok()?;
    Some(returned as usize)
}
