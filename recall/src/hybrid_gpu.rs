//! Hybrid-laptop GPU choice. windows-capture creates its D3D11 device on the default
//! adapter. On an NVIDIA Optimus or AMD switchable laptop that is the integrated GPU, and
//! NVENC cannot take frames from it, so recording would fall back to CPU copies (a full
//! readback per frame plus a polling redraw loop). These two exported symbols are the
//! drivers' documented opt-in: the process's default adapter becomes the discrete GPU.
//! `build.rs` exports them from the engine binary. They change nothing on a desktop with
//! a single discrete GPU.

#![allow(non_upper_case_globals)]

/// NVIDIA Optimus: 1 = run on the high-performance GPU.
#[no_mangle]
#[used]
pub static NvOptimusEnablement: u32 = 1;

/// AMD PowerXpress / switchable graphics: 1 = run on the high-performance GPU.
#[no_mangle]
#[used]
pub static AmdPowerXpressRequestHighPerformance: u32 = 1;
