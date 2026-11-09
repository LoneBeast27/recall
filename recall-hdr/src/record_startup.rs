//! Controller start-up helpers: process niceness (CPU mask + priority) and the
//! window pinned from the environment (split out of `record_app`).

use windows::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS};

use crate::log;
use crate::record_config::parse_cpu_mask;
use crate::record_env::apply_affinity;
use crate::record_target::{display_name, sanitize, Target};
use crate::watcher;

const FALLBACK_CPU_MASK: usize = crate::record_config::AUTO_CPU_MASK;

/// Apply the CPU mask + BelowNormal priority to this process (children inherit
/// both at creation). An invalid or unsupported mask is logged, never fatal.
/// Returns the mask (hex text) now in effect, if any.
pub(crate) fn apply_niceness(mask_text: &str) -> Option<String> {
    let mask = parse_cpu_mask(mask_text).unwrap_or_else(|e| {
        log!("record: {e}; using 0x{FALLBACK_CPU_MASK:x}");
        FALLBACK_CPU_MASK
    });
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
    apply_affinity(mask)
}

/// `Start-Recall.ps1 -Valorant`: `RECALL_CAPTURE_HWND` pins one window (name
/// from `RECALL_CAPTURE_NAME`, audio pid from `RECALL_AUDIO_GAME_PID`) until
/// its process exits.
pub(crate) fn pinned_target_from_env() -> Option<Target> {
    let raw = std::env::var("RECALL_CAPTURE_HWND").ok()?;
    let Ok(hwnd) = raw.trim().parse::<usize>() else {
        log!("record: RECALL_CAPTURE_HWND={raw:?} is not a window handle; ignoring the pin");
        return None;
    };
    let hwnd = hwnd as isize;
    let Some((window_pid, stem, exe, title)) = watcher::window_identity(hwnd) else {
        log!("record: RECALL_CAPTURE_HWND={hwnd} is not a live window; ignoring the pin");
        return None;
    };
    let pid = std::env::var("RECALL_AUDIO_GAME_PID")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|p| *p != 0)
        .unwrap_or(window_pid);
    let name = std::env::var("RECALL_CAPTURE_NAME")
        .ok()
        .map(|n| sanitize(&n))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| display_name(&title, &stem));
    log!("record: pinned target from environment: {name:?} ({exe}, pid {pid})");
    Some(Target { hwnd, pid, name, exe })
}
