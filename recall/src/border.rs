//! Best-effort suppression of the Windows capture border — the colored privacy
//! frame WGC draws around captured items.
//!
//! windows-capture already calls `SetIsBorderRequired(false)` for our
//! `WithoutBorder` setting, but Windows only honors it once the process has been
//! granted **Borderless** capture access. We request that here. It's harmless if
//! denied (policy-restricted / older build) — the border simply remains.

use windows::Graphics::Capture::{GraphicsCaptureAccess, GraphicsCaptureAccessKind};
use windows::Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

/// Request process-wide Borderless capture access. Blocks briefly; runs on a
/// throwaway MTA thread so it can't disturb the main thread's COM apartment, and
/// completes before the capture session applies `IsBorderRequired(false)`.
pub fn request_borderless() {
    let _ = std::thread::spawn(|| unsafe {
        // Ignore RPC_E_CHANGED_MODE etc. — we only need an initialized apartment.
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
        match GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
            .and_then(|op| op.join())
        {
            Ok(AppCapabilityAccessStatus::Allowed) => {
                eprintln!("[recall] borderless capture access: granted");
            }
            Ok(status) => {
                eprintln!("[recall] borderless capture access: not granted ({status:?})");
            }
            Err(e) => {
                eprintln!("[recall] borderless capture access request failed: {e}");
            }
        }
    })
    .join();
}
