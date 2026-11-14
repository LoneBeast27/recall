//! A D3D11 fence the capture thread waits on before it hands the capture surface back.

use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// A D3D11 fence + event: the capture thread sleeps until the GPU passes the signal
/// (typically 0.05-1.5 ms). Without D3D11.4 (no fence) it does not wait.
pub(super) struct GpuDone {
    fence: Option<(ID3D11Fence, HANDLE)>,
    value: u64,
    armed: bool,
}

impl GpuDone {
    pub(super) fn new(device: &ID3D11Device) -> Self {
        let fence = device.cast::<ID3D11Device5>().ok().and_then(|d5| unsafe {
            let mut fence: Option<ID3D11Fence> = None;
            d5.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence).ok()?;
            let fence = fence?;
            let event = CreateEventW(None, false, false, None).ok()?;
            Some((fence, event))
        });
        Self { fence, value: 0, armed: false }
    }

    /// Queue the signal after the frame's work (the caller flushes).
    pub(super) fn signal(&mut self, context: &ID3D11DeviceContext) {
        self.armed = false;
        let Some((fence, _)) = &self.fence else { return };
        let Ok(ctx4) = context.cast::<ID3D11DeviceContext4>() else { return };
        self.value += 1;
        self.armed = unsafe { ctx4.Signal(fence, self.value) }.is_ok();
    }

    pub(super) fn wait(&mut self) {
        let Some((fence, event)) = &self.fence else { return };
        if !self.armed {
            return;
        }
        unsafe {
            if fence.SetEventOnCompletion(self.value, *event).is_ok() {
                // Bounded: a hung GPU must not stall capture forever.
                WaitForSingleObject(*event, 100);
            }
        }
    }
}

impl Drop for GpuDone {
    fn drop(&mut self) {
        if let Some((_, event)) = self.fence.take() {
            let _ = unsafe { CloseHandle(event) };
        }
    }
}

