//! VENDOR SEAM (NVIDIA only): RTX Video Super Resolution through the D3D11 video
//! processor stream extension the driver exposes (the same path browsers and
//! video players use). Nothing is redistributed: the model ships in the driver.
//! The driver accepts the call even when its "Super resolution" switch is off,
//! so callers check the result with `video_processor::probe_readiness`.

use windows::core::GUID;
use windows::Win32::Graphics::Direct3D11::{ID3D11VideoContext, ID3D11VideoProcessor};

use super::SuperResolution;

pub(super) const PCI_VENDOR_ID: u32 = 0x10DE;

/// NVIDIA's post-processing stream extension.
const PPE_INTERFACE: GUID = GUID::from_u128(0xd43ce1b3_1f4b_48ac_baee_c3c25375e6f7);
const EXTENSION_VERSION_V1: u32 = 0x1;
const METHOD_SUPER_RESOLUTION: u32 = 0x2;

#[repr(C)]
struct StreamExtension {
    version: u32,
    method: u32,
    enable: u32,
}

pub(super) struct RtxVideoSuperResolution;

impl SuperResolution for RtxVideoSuperResolution {
    fn vendor(&self) -> &'static str {
        "NVIDIA"
    }

    fn set(&self, context: &ID3D11VideoContext, processor: &ID3D11VideoProcessor, on: bool) -> Result<(), String> {
        let mut ext = StreamExtension { version: EXTENSION_VERSION_V1, method: METHOD_SUPER_RESOLUTION, enable: on as u32 };
        let hr = unsafe {
            context.VideoProcessorSetStreamExtension(
                processor,
                0,
                &PPE_INTERFACE,
                std::mem::size_of::<StreamExtension>() as u32,
                &mut ext as *mut StreamExtension as *mut _,
            )
        };
        if hr < 0 {
            return Err(format!("RTX Video Super Resolution: driver refused (0x{:08X})", hr as u32));
        }
        Ok(())
    }

    /// NVIDIA's VSR model takes 640x360 to 3840x2160 inputs (NGC model card, "dlpp").
    fn min_input(&self) -> (u32, u32) {
        (640, 360)
    }

    fn requirement(&self) -> &'static str {
        "RTX 20 series or newer cards"
    }

    fn driver_setting(&self) -> &'static str {
        "NVIDIA app > System > Video > RTX Video Enhancement > Super resolution"
    }
}
