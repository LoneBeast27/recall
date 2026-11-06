//! DXGI and D3D11 plumbing for the Desktop Duplication capture (`dda`): finding the output
//! that shows a monitor, the device on its adapter, the CPU-readback staging texture, the
//! QPC clock and a high-resolution wait.

use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Performance::QueryPerformanceCounter;

use super::CaptureError;
use crate::framing::Rect;

/// The output that shows a monitor and the adapter that drives it.
pub(super) struct Target {
    pub adapter: IDXGIAdapter1,
    pub output: IDXGIOutput1,
    pub adapter_name: String,
    pub luid: (u32, i32),
}

/// Find the output showing `monitor`. Its adapter must be the one WGC (and so the encoder
/// path) would use, the process default adapter: on a hybrid laptop where the iGPU drives
/// the panel and the dGPU encodes (`hybrid_gpu`), DDA would need a cross-adapter copy, so
/// those stay on WGC.
pub(super) fn find_target(monitor: HMONITOR) -> Result<Target, String> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| format!("DXGI factory: {e}"))?;
    let default = unsafe { factory.EnumAdapters1(0) }.and_then(|a| unsafe { a.GetDesc1() }).map_err(|e| format!("default adapter: {e}"))?;
    for a in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(a) }) else { break };
        for o in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(o) }) else { break };
            let Ok(desc) = (unsafe { output.GetDesc() }) else { continue };
            if desc.Monitor != monitor {
                continue;
            }
            let ad = unsafe { adapter.GetDesc1() }.map_err(|e| format!("adapter desc: {e}"))?;
            let name = wide(&ad.Description);
            if ad.AdapterLuid != default.AdapterLuid {
                return Err(format!("monitor is driven by {name}, the encoder device is on {} (hybrid GPU)", wide(&default.Description)));
            }
            let output = output.cast::<IDXGIOutput1>().map_err(|e| format!("IDXGIOutput1: {e}"))?;
            return Ok(Target { adapter, output, adapter_name: name, luid: luid(ad.AdapterLuid) });
        }
    }
    Err("no DXGI output shows this monitor".into())
}

fn luid(l: LUID) -> (u32, i32) {
    (l.LowPart, l.HighPart)
}

pub(super) fn device_on(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|e| format!("D3D11 device: {e}"))?;
    let (device, context) = (device.ok_or("no device")?, context.ok_or("no context")?);
    // The encoder and screenshot readbacks share the device across threads (as on WGC).
    if let Ok(mt) = context.cast::<ID3D11Multithread>() {
        let _ = unsafe { mt.SetMultithreadProtected(true) };
    }
    Ok((device, context))
}

fn wide(s: &[u16]) -> String {
    String::from_utf16_lossy(&s[..s.iter().position(|c| *c == 0).unwrap_or(s.len())])
}

/// The present time, or now when the frame carries none (QPC ticks).
pub(super) fn present_or_now(last_present_time: i64) -> i64 {
    if last_present_time != 0 {
        return last_present_time;
    }
    let mut now = 0i64;
    let _ = unsafe { QueryPerformanceCounter(&mut now) };
    now
}

/// The CPU-readback staging texture (made again on a size change).
pub(super) struct Readback {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    format: DXGI_FORMAT,
    staging: Option<(ID3D11Texture2D, (u32, u32))>,
}

impl Readback {
    pub(super) fn new(device: ID3D11Device, context: ID3D11DeviceContext, format: DXGI_FORMAT) -> Self {
        Self { device, context, format, staging: None }
    }

    /// CPU path only (no encoder GPU sink, or the dump harness): map a staging copy and copy
    /// out `rect` of the fp16 frame (8 bytes/pixel), as WGC's `frame.buffer()` path does.
    pub(super) fn read(&mut self, src: &ID3D11Texture2D, size: (u32, u32), rect: Rect) -> Result<Vec<u8>, CaptureError> {
        if self.staging.as_ref().is_none_or(|(_, s)| *s != size) {
            let want = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: self.format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut tex = None;
            unsafe { self.device.CreateTexture2D(&want, None, Some(&mut tex))? };
            self.staging = Some((tex.ok_or("staging texture missing")?, size));
        }
        let (staging, _) = self.staging.as_ref().ok_or("staging texture missing")?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context.CopyResource(staging, src);
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let pitch = mapped.RowPitch as usize;
        let data = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * size.1 as usize) };
        let bytes = crate::framing::copy_rect(data, pitch, 8, rect);
        unsafe { self.context.Unmap(staging, 0) };
        Ok(bytes)
    }
}

/// A high-resolution waitable timer (plain sleep rounds up to the 15.6 ms system tick, a
/// whole frame at 60 fps), closed on drop.
pub(super) struct Timer(Option<HANDLE>);

impl Timer {
    pub(super) fn new() -> Self {
        use windows::Win32::System::Threading::{CreateWaitableTimerExW, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS};
        Self(unsafe { CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0) }.ok())
    }

    pub(super) fn wait(&self, wait: std::time::Duration) {
        use windows::Win32::System::Threading::{SetWaitableTimer, WaitForSingleObject, INFINITE};
        if wait.is_zero() {
            return;
        }
        let due = -((wait.as_nanos() / 100) as i64).max(1); // negative = relative, 100 ns units
        let waited = match self.0 {
            Some(h) => unsafe { SetWaitableTimer(h, &due, 0, None, None, false).is_ok() && WaitForSingleObject(h, INFINITE).0 == 0 },
            None => false,
        };
        if !waited {
            std::thread::sleep(wait);
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            let _ = unsafe { CloseHandle(h) };
        }
    }
}
