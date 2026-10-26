//! NVIDIA GPU usage and temperature through NVML. `nvml.dll` ships with the
//! driver in System32; it is loaded at runtime only while the stats island is
//! on (no link-time dependency, unloaded with the poller).

use std::ffi::c_void;

use windows::core::{s, w, PCSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

const NVML_SUCCESS: i32 = 0;
const NVML_TEMPERATURE_GPU: u32 = 0;

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: u32,
    memory: u32,
}

type InitFn = unsafe extern "C" fn() -> i32;
type ShutdownFn = unsafe extern "C" fn() -> i32;
type HandleByIndexFn = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
type UtilizationFn = unsafe extern "C" fn(*mut c_void, *mut Utilization) -> i32;
type TemperatureFn = unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuSample {
    /// Percent of time a kernel ran over NVML's last sample period.
    pub usage: u32,
    pub temp_c: Option<u32>,
}

pub struct Nvml {
    module: HMODULE,
    shutdown: ShutdownFn,
    utilization: UtilizationFn,
    temperature: TemperatureFn,
    device: *mut c_void,
}

impl Nvml {
    /// Loads NVML and opens GPU 0; `None` without an NVIDIA driver.
    pub fn open() -> Option<Self> {
        unsafe {
            let module = LoadLibraryExW(w!("nvml.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
            match Self::bind(module) {
                Some(nvml) => Some(nvml),
                None => {
                    let _ = FreeLibrary(module);
                    None
                }
            }
        }
    }

    unsafe fn bind(module: HMODULE) -> Option<Self> {
        let symbol = |name: PCSTR| unsafe { GetProcAddress(module, name) };
        let init: InitFn = unsafe { std::mem::transmute(symbol(s!("nvmlInit_v2"))?) };
        let shutdown: ShutdownFn = unsafe { std::mem::transmute(symbol(s!("nvmlShutdown"))?) };
        let by_index: HandleByIndexFn = unsafe { std::mem::transmute(symbol(s!("nvmlDeviceGetHandleByIndex_v2"))?) };
        let utilization: UtilizationFn = unsafe { std::mem::transmute(symbol(s!("nvmlDeviceGetUtilizationRates"))?) };
        let temperature: TemperatureFn = unsafe { std::mem::transmute(symbol(s!("nvmlDeviceGetTemperature"))?) };
        if unsafe { init() } != NVML_SUCCESS {
            return None;
        }
        let mut device = std::ptr::null_mut();
        if unsafe { by_index(0, &mut device) } != NVML_SUCCESS || device.is_null() {
            unsafe { shutdown() };
            return None;
        }
        Some(Self { module, shutdown, utilization, temperature, device })
    }

    pub fn sample(&self) -> Option<GpuSample> {
        let mut rates = Utilization::default();
        if unsafe { (self.utilization)(self.device, &mut rates) } != NVML_SUCCESS {
            return None;
        }
        let mut temp = 0u32;
        let temp_ok = unsafe { (self.temperature)(self.device, NVML_TEMPERATURE_GPU, &mut temp) } == NVML_SUCCESS;
        Some(GpuSample { usage: rates.gpu.min(100), temp_c: temp_ok.then_some(temp) })
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
            let _ = FreeLibrary(self.module);
        }
    }
}
