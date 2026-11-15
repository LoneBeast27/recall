//! GPU scheduling priority of Recall's capture work (test switches while it is measured; no
//! change unless one is set). Plain Windows calls, no vendor code.
//!
//! - `RECALL_GPU_PRIORITY` (-7..=0): `IDXGIDevice::SetGPUThreadPriority` of the capture device,
//!   which carries the conversion and NVENC's input. Below 0 the game's GPU work goes first.
//! - `RECALL_GPU_PROCESS_CLASS` (`idle` | `below`): `D3DKMTSetProcessSchedulingPriorityClass`
//!   for the whole recorder process (the call ShadowPlay's service makes for its capture
//!   process). Lowering needs no privilege.

use windows::core::Interface;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

#[link(name = "gdi32")]
unsafe extern "system" {
    fn D3DKMTSetProcessSchedulingPriorityClass(process: HANDLE, class: i32) -> i32;
}

/// D3DKMT_SCHEDULINGPRIORITY_CLASS values.
fn class(name: &str) -> Option<i32> {
    match name.trim() {
        "idle" => Some(0),
        "below" => Some(1),
        _ => None,
    }
}

pub(super) fn apply(device: &ID3D11Device) {
    if let Some(priority) = std::env::var("RECALL_GPU_PRIORITY").ok().and_then(|v| v.trim().parse::<i32>().ok()).map(|p| p.clamp(-7, 0)) {
        let set = device
            .cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice>()
            .and_then(|dxgi| unsafe { dxgi.SetGPUThreadPriority(priority) });
        eprintln!("[recall] capture device GPU priority {priority}: {}", if set.is_ok() { "set" } else { "refused" });
    }
    if let Some(c) = std::env::var("RECALL_GPU_PROCESS_CLASS").ok().as_deref().and_then(class) {
        let status = unsafe { D3DKMTSetProcessSchedulingPriorityClass(windows::Win32::System::Threading::GetCurrentProcess(), c) };
        eprintln!("[recall] process GPU scheduling class {c}: status {status:#x}");
    }
}
