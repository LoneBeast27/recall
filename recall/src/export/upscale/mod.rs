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
    /// The driver takes the request but leaves the picture unchanged: its switch is off, or
    /// the card has no such upscaler (a GTX card can answer the same way). Recall never
    /// changes driver settings, so the UI says which cards have it and where it is turned on.
    NeedsDriverSetting { requirement: &'static str, setting: &'static str },
    /// The clip is smaller than the AI upscaler takes (`SuperResolution::min_input`): exports
    /// use FSR 1 whatever the driver switch says.
    TooSmall { min: (u32, u32) },
    /// This GPU or driver has no super resolution: exports use FSR 1.
    Unsupported,
}

impl Readiness {
    /// The notice text the export panel shows, or None when nothing is needed.
    pub(crate) fn notice(&self) -> Option<String> {
        match self {
            Readiness::Ready => None,
            Readiness::NeedsDriverSetting { requirement, setting } => Some(format!(
                "AI upscaling isn't running, so Recall upscales with AMD FSR 1. On {requirement}, turn on {setting} for AI upscaling."
            )),
            Readiness::TooSmall { min } => Some(format!(
                "This clip is smaller than {}x{}, the smallest picture the AI upscaler takes, so Recall upscales it with AMD FSR 1.",
                min.0, min.1
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
    /// Which of the vendor's cards have it, for the notice ("RTX 20 series or newer cards").
    fn requirement(&self) -> &'static str;
    /// The smallest input picture it upscales; smaller ones are left as the plain scale.
    fn min_input(&self) -> (u32, u32);
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
/// upscaler is ready), for a `input` -> `output` upscale. Some GPU work on a new device, no
/// window: the app asks only when the user opens the Upscale row (`--upscale-notice`).
pub(crate) fn current_notice(input: (u32, u32), output: (u32, u32)) -> Option<String> {
    // The same adapter order as an export: the encoder's vendor first.
    let first = super::gpu::adapters(Some(super::video_encoder::NVENC_VENDOR)).ok().and_then(|list| list.into_iter().next());
    match first.map(|adapter| super::gpu::video_device(&adapter)) {
        Some(Ok((device, _))) => probe_readiness(&device, for_adapter(adapter_vendor(&device)), input, output).notice(),
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
    let (tier, why) = pick_tier(device, context, input, out);
    let name = match tier {
        Tier::Ai(ref sr) => format!("{} AI super resolution", sr.vendor()),
        Tier::Fsr(_) => "AMD FSR 1".to_owned(),
        Tier::Plain => "the plain video processor scale".to_owned(),
    };
    eprintln!("[recall] upscale {}x{} -> {}x{}: {name} ({why})", input.0, input.1, out.0, out.1);
    tier
}

fn pick_tier(device: &ID3D11Device, context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext, input: (u32, u32), out: (u32, u32)) -> (Tier, String) {
    let vendor = adapter_vendor(device);
    // Checks force a tier: `RECALL_UPSCALER=ai|fsr|plain`.
    let forced = std::env::var("RECALL_UPSCALER").unwrap_or_default();
    let ai_why = match forced.trim() {
        "plain" => return (Tier::Plain, "forced by RECALL_UPSCALER".into()),
        "ai" => return (for_adapter(vendor).map_or(Tier::Plain, Tier::Ai), "forced by RECALL_UPSCALER".into()),
        "fsr" => "FSR 1 forced by RECALL_UPSCALER".to_owned(),
        _ => match probe_readiness(device, for_adapter(vendor), input, out) {
            Readiness::Ready => match for_adapter(vendor) {
                Some(sr) => return (Tier::Ai(sr), "checked: it changes this picture".into()),
                None => "no AI upscaler".into(),
            },
            Readiness::NeedsDriverSetting { .. } => "AI upscaler left the picture unchanged (driver switch off, or no such upscaler)".into(),
            Readiness::TooSmall { min } => format!("the clip is under {}x{}, the AI upscaler's smallest input", min.0, min.1),
            Readiness::Unsupported => "no AI upscaler on this GPU".into(),
        },
    };
    match fsr1::Fsr1::new(device, context.clone(), input, out) {
        Ok(fsr) => (Tier::Fsr(Box::new(fsr)), ai_why),
        Err(why) => (Tier::Plain, format!("{ai_why}; FSR 1 unavailable: {why}")),
    }
}
