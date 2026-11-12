//! Upscaled export: an opt-in exception to "export never exceeds the recording".
//!
//! Only a clip recorded below the source's native height may be exported above
//! its recorded height, and never above native. The scale runs on the GPU in the
//! D3D11 video processor, between hardware decode and NVENC.
//!
//! VENDOR SEAM: everything vendor-specific lives in `vendor_*` modules behind
//! [`SuperResolution`]. This package ships NVIDIA only (`vendor_nvidia`); an AMD
//! or Intel package adds its own module and picks it in [`for_adapter`]. The
//! plain scale ([`VideoProcessorScaler`] with no super resolution) is vendor-neutral and is the fallback everywhere.

mod size;
mod vendor_nvidia;
mod video_processor;

#[cfg(test)]
mod size_tests;

pub(crate) use size::{upscale_choices, upscaled_dims};
pub(crate) use video_processor::{probe_readiness, VideoProcessorScaler};

use windows::Win32::Graphics::Direct3D11::{ID3D11VideoContext, ID3D11VideoProcessor};

/// Whether an AI upscaler can be used right now, and what the user must do if not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Readiness {
    /// The driver applies super resolution: exports use it.
    Ready,
    /// The driver takes the request but leaves the picture unchanged. Recall never
    /// changes driver settings, so the UI tells the user where to turn it on.
    NeedsDriverSetting { vendor: &'static str, setting: &'static str },
    /// This GPU or driver has no super resolution: exports use the plain scale.
    Unsupported,
}

impl Readiness {
    /// The notice text the export panel shows, or None when nothing is needed.
    pub(crate) fn notice(&self) -> Option<String> {
        match self {
            Readiness::Ready => None,
            Readiness::NeedsDriverSetting { vendor, setting } => Some(format!(
                "AI upscaling is off in the {vendor} driver. Turn on {setting}, then export again. Until then Recall uses a plain upscale."
            )),
            Readiness::Unsupported => Some("This GPU has no AI upscaler. Recall uses a plain upscale.".into()),
        }
    }
}

/// VENDOR SEAM: one GPU vendor's super resolution on the D3D11 video processor.
pub(crate) trait SuperResolution: Send {
    /// Short vendor name for notices and logs.
    fn vendor(&self) -> &'static str;
    /// Turns super resolution on or off for stream 0 of `processor`.
    fn set(&self, context: &ID3D11VideoContext, processor: &ID3D11VideoProcessor, on: bool) -> Result<(), String>;
    /// Where the user turns the driver feature on, in the vendor's own app.
    fn driver_setting(&self) -> &'static str;
}

/// VENDOR SEAM: picks the super resolution for an adapter's PCI vendor id.
/// AMD (0x1002) and Intel (0x8086) packages add their arms here.
pub(crate) fn for_adapter(vendor_id: u32) -> Option<Box<dyn SuperResolution>> {
    match vendor_id {
        vendor_nvidia::PCI_VENDOR_ID => Some(Box::new(vendor_nvidia::RtxVideoSuperResolution)),
        _ => None,
    }
}
