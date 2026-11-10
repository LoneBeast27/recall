//! Tests that need NVENC or a hardware D3D11 device say so when they cannot run: each skip
//! is written straight to stderr (past the test harness's output capture) with a running
//! count, so a green run on a machine without the hardware is visibly not a real pass.
//!
//! `RECALL_REQUIRE_NVENC=1` turns every skip into a failure (a gate run on the GPU box);
//! `RECALL_FAKE_NO_NVENC=1` makes the hardware look absent (to check the skip path).

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

static SKIPS: AtomicUsize = AtomicUsize::new(0);

/// The hardware is to be treated as absent (`RECALL_FAKE_NO_NVENC=1`).
pub(crate) fn faked_absent() -> bool {
    std::env::var_os("RECALL_FAKE_NO_NVENC").is_some_and(|v| v == "1")
}

/// A hardware D3D11 device for a GPU test, or `None` after a counted skip that names why.
/// On a GitHub runner, or a VM or remote session without a GPU, D3D11 hands out a "hardware"
/// device on the Microsoft Basic Render Driver (vendor 0x1414). It has no NVENC, and FFmpeg's
/// D3D11 device cannot open on it, so that counts as absent, with the adapter in the skip line.
pub(crate) fn gpu_device() -> Option<(
    windows::Win32::Graphics::Direct3D11::ID3D11Device,
    windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
)> {
    if faked_absent() {
        skipped("no hardware D3D11 device (RECALL_FAKE_NO_NVENC=1)");
        return None;
    }
    let Some((dev, ctx)) = crate::gpu_frames::hardware_device() else {
        skipped("no hardware D3D11 device");
        return None;
    };
    if let Some((a, _)) = crate::gpu_frames::adapter::device_adapter(&dev).filter(|(a, _)| a.vendor == 0x1414) {
        skipped(&format!("only a software D3D11 adapter: {} [vendor 0x{:04x}]", a.name, a.vendor));
        return None;
    }
    Some((dev, ctx))
}

/// The line a skip prints: `HW-SKIP #<n> <test>: <why>`.
pub(crate) fn skip_line(n: usize, test: &str, why: &str) -> String {
    format!("HW-SKIP #{n} {test}: {why} (counted; not a pass)")
}

/// Record that the calling test could not run on this machine.
pub(crate) fn skipped(why: &str) {
    let test = std::thread::current().name().unwrap_or("<test>").to_owned();
    if std::env::var_os("RECALL_REQUIRE_NVENC").is_some_and(|v| v == "1") {
        panic!("{test}: {why} (RECALL_REQUIRE_NVENC=1)");
    }
    let n = SKIPS.fetch_add(1, Relaxed) + 1;
    let _ = writeln!(std::io::stderr().lock(), "{}", skip_line(n, &test, why));
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_skip_names_the_test_and_its_count() {
        assert_eq!(super::skip_line(2, "encoder::x", "no NVENC"), "HW-SKIP #2 encoder::x: no NVENC (counted; not a pass)");
    }
}
