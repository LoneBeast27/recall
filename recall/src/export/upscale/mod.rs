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

mod fsr1;
mod size;
mod vendor_nvidia;
mod video_processor;

#[cfg(test)]
mod size_tests;

pub(crate) use size::planning_dims;
#[cfg(test)]
pub(crate) use size::{upscale_choices, upscaled_dims};
use video_processor::probe_readiness;
#[cfg(test)]
pub(crate) use video_processor::VideoProcessorScaler;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11VideoContext, ID3D11VideoProcessor};

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
                "AI upscaling is off in the {vendor} driver. Turn on {setting}, then export again. Until then Recall upscales with AMD FSR 1."
            )),
            Readiness::Unsupported => Some("This GPU has no AI upscaler. Recall upscales with AMD FSR 1.".into()),
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

/// PCI vendor id of the device's adapter (0 when it cannot be read).
pub(crate) fn adapter_vendor(device: &ID3D11Device) -> u32 {
    let adapter = device.cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice>().and_then(|d| unsafe { d.GetAdapter() });
    adapter.and_then(|a| unsafe { a.GetDesc() }).map_or(0, |desc| desc.VendorId)
}

/// The notice for an upscaled export on the GPU exports try first (None when the AI
/// upscaler is ready). A few ms of GPU work on a new device; no window.
pub(crate) fn current_notice() -> Option<String> {
    let first = super::gpu::adapters(None).ok().and_then(|list| list.into_iter().next());
    match first.map(|adapter| super::gpu::video_device(&adapter)) {
        Some(Ok((device, _))) => probe_readiness(&device, for_adapter(adapter_vendor(&device))).notice(),
        _ => Readiness::Unsupported.notice(),
    }
}

/// How an upscaled export scales, best first. Every NVIDIA card from GTX 10 up gets FSR 1
/// at least; RTX cards with the driver switch on get the AI upscaler.
pub(crate) enum Tier {
    /// A vendor's AI super resolution inside the export's own video processor.
    Ai(Box<dyn SuperResolution>),
    /// FSR 1 compute passes (any D3D11 GPU).
    Fsr(Box<fsr1::Fsr1>),
    /// The video processor's own scale.
    Plain,
}

/// The best tier this device runs for an `input` -> `out` upscale.
pub(crate) fn pick(device: &ID3D11Device, context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext, input: (u32, u32), out: (u32, u32)) -> Tier {
    let vendor = adapter_vendor(device);
    // Checks force a tier: `RECALL_UPSCALER=ai|fsr|plain`.
    let forced = std::env::var("RECALL_UPSCALER").unwrap_or_default();
    match forced.trim() {
        "plain" => return Tier::Plain,
        "ai" => return for_adapter(vendor).map_or(Tier::Plain, Tier::Ai),
        _ => {}
    }
    if forced.trim() != "fsr" && probe_readiness(device, for_adapter(vendor)) == Readiness::Ready {
        if let Some(sr) = for_adapter(vendor) {
            return Tier::Ai(sr);
        }
    }
    match fsr1::Fsr1::new(device, context.clone(), input, out) {
        Ok(fsr) => Tier::Fsr(Box::new(fsr)),
        Err(why) => {
            eprintln!("[recall] FSR 1 unavailable ({why}); plain upscale");
            Tier::Plain
        }
    }
}
