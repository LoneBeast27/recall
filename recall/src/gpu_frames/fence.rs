//! A D3D11 fence + event for one-off GPU work a thread waits on (the HUD probe's readback):
//! the thread sleeps until the GPU passes the signal instead of polling a map. The same
//! pattern as the capture pipeline's per-frame wait (`pipeline.rs` `GpuDone`). Without
//! D3D11.4 (no `ID3D11Device5` fence) it is not available and callers keep their fallback.

use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Device5, ID3D11DeviceContext, ID3D11DeviceContext4, ID3D11Fence, D3D11_FENCE_FLAG_NONE};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

pub(super) struct GpuFence {
    fence: ID3D11Fence,
    event: HANDLE,
    value: u64,
}

// The fence is a free-threaded D3D11 object and the event a kernel handle; one owner thread
// uses them at a time (the struct is moved, never shared).
unsafe impl Send for GpuFence {}

impl GpuFence {
    /// None without D3D11.4 (or when the fence or event cannot be made).
    pub(super) fn new(device: &ID3D11Device) -> Option<Self> {
        let d5 = device.cast::<ID3D11Device5>().ok()?;
        unsafe {
            let mut fence: Option<ID3D11Fence> = None;
            d5.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence).ok()?;
            let fence = fence?;
            let event = CreateEventW(None, false, false, None).ok()?;
            Some(Self { fence, event, value: 0 })
        }
    }

    /// Queue the signal after the work recorded so far on `context` (the caller flushes, under
    /// the device lock). False: not queued, do not `wait`.
    pub(super) fn signal(&mut self, context: &ID3D11DeviceContext) -> bool {
        let Ok(ctx4) = context.cast::<ID3D11DeviceContext4>() else { return false };
        self.value += 1;
        unsafe { ctx4.Signal(&self.fence, self.value) }.is_ok()
    }

    /// Sleep until the GPU passed the last signal, at most `timeout_ms`. True: it did.
    pub(super) fn wait(&self, timeout_ms: u32) -> bool {
        unsafe {
            if self.fence.GetCompletedValue() >= self.value {
                return true;
            }
            self.fence.SetEventOnCompletion(self.value, self.event).is_ok() && WaitForSingleObject(self.event, timeout_ms) == WAIT_OBJECT_0
        }
    }
}

impl Drop for GpuFence {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.event) };
    }
}
