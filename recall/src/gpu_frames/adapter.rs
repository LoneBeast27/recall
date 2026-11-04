//! Which GPU the capture device runs on, and how much VRAM this process holds there.
//!
//! The capture device is remembered by its adapter LUID when the zero-copy pipeline is
//! built (`remember`). Later readings (`process_vram_mb`) ask DXGI for this process's
//! local-memory use on that adapter, so the engine log shows what each stage allocates.
//! `low_end` marks NVIDIA GPUs without "RTX" in the name (GTX 10/16, MX, Quadro P): one
//! older NVENC unit, a few GB of VRAM.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use windows::core::Interface;
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter3, IDXGIDevice, IDXGIFactory4, DXGI_ADAPTER_DESC, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
    DXGI_QUERY_VIDEO_MEMORY_INFO,
};

const NVIDIA: u32 = 0x10DE;

/// LUID of the capture adapter (low 32 bits | high 32 bits << 32); 0 = not known yet.
static LUID_BITS: AtomicU64 = AtomicU64::new(0);
static LOW_END: AtomicBool = AtomicBool::new(false);

/// Name, vendor id and dedicated VRAM of a GPU, as DXGI reports them.
#[derive(Clone, Debug, PartialEq)]
pub struct AdapterInfo {
    pub name: String,
    pub vendor: u32,
    pub dedicated_mb: u64,
}

impl AdapterInfo {
    fn from_desc(desc: &DXGI_ADAPTER_DESC) -> Self {
        let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        Self {
            name: String::from_utf16_lossy(&desc.Description[..end]),
            vendor: desc.VendorId,
            dedicated_mb: desc.DedicatedVideoMemory as u64 / (1024 * 1024),
        }
    }

    /// An NVIDIA GPU without "RTX" in its name: GTX 10/16, MX, Quadro P and older.
    pub fn is_low_end(&self) -> bool {
        self.vendor == NVIDIA && !self.name.to_ascii_uppercase().contains("RTX")
    }

    /// One log line: name, vendor, VRAM, and whether NVENC can take its frames.
    pub fn describe(&self) -> String {
        let note = if self.vendor != NVIDIA {
            " (not NVIDIA: NVENC cannot take its frames)"
        } else if self.is_low_end() {
            " (low-end NVENC: preset capped at p4)"
        } else {
            ""
        };
        format!("{} [vendor 0x{:04x}, {} MB VRAM]{note}", self.name, self.vendor, self.dedicated_mb)
    }
}

/// The adapter `device` runs on.
pub fn device_adapter(device: &ID3D11Device) -> Option<(AdapterInfo, LUID)> {
    let dxgi = device.cast::<IDXGIDevice>().ok()?;
    let adapter = unsafe { dxgi.GetAdapter() }.ok()?;
    let desc = unsafe { adapter.GetDesc() }.ok()?;
    Some((AdapterInfo::from_desc(&desc), desc.AdapterLuid))
}

/// Remember the capture device's adapter for later VRAM readings and the preset choice.
pub fn remember(device: &ID3D11Device) -> Option<AdapterInfo> {
    let (info, luid) = device_adapter(device)?;
    LUID_BITS.store(u64::from(luid.LowPart) | (u64::from(luid.HighPart as u32) << 32), Relaxed);
    LOW_END.store(info.is_low_end(), Relaxed);
    Some(info)
}

/// True once the capture device is known to sit on a low-end NVIDIA GPU.
pub fn low_end() -> bool {
    LOW_END.load(Relaxed)
}

/// This process's local (dedicated) VRAM use on the capture adapter, in MB.
pub fn process_vram_mb() -> Option<u64> {
    let bits = LUID_BITS.load(Relaxed);
    if bits == 0 {
        return None;
    }
    let luid = LUID { LowPart: bits as u32, HighPart: (bits >> 32) as i32 };
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory1().ok()?;
        let adapter: IDXGIAdapter3 = factory.EnumAdapterByLuid(luid).ok()?;
        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info).ok()?;
        Some(info.CurrentUsage / (1024 * 1024))
    }
}

/// "<stage>: <n> MB VRAM" for the log, or `None` before the adapter is known.
pub fn vram_line(stage: &str) -> Option<String> {
    process_vram_mb().map(|mb| format!("vram {stage}: {mb} MB"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(name: &str, vendor: u32) -> AdapterInfo {
        AdapterInfo { name: name.into(), vendor, dedicated_mb: 4096 }
    }

    #[test]
    fn gtx_and_mx_are_low_end_and_rtx_is_not() {
        assert!(gpu("NVIDIA GeForce GTX 1650", NVIDIA).is_low_end());
        assert!(gpu("NVIDIA GeForce MX450", NVIDIA).is_low_end());
        assert!(!gpu("NVIDIA GeForce RTX 5090", NVIDIA).is_low_end());
        assert!(!gpu("NVIDIA GeForce RTX 3050 Laptop GPU", NVIDIA).is_low_end());
        assert!(!gpu("Intel(R) UHD Graphics", 0x8086).is_low_end());
    }

    #[test]
    fn describe_warns_about_an_integrated_gpu() {
        assert!(gpu("AMD Radeon(TM) Graphics", 0x1002).describe().contains("not NVIDIA"));
        assert!(gpu("NVIDIA GeForce GTX 1650", NVIDIA).describe().contains("p4"));
    }
}
