//! recall-hdr: Recall's controller (Windows, RTX 5090).
//!
//! Entry point. Modes:
//! - (no args), `--record`, `--record-only` : the unified record mode
//!   ([`record_app::run`]) -- records any game automatically, tray, hotkeys,
//!   control pipe, overlay; HDR management only with `behavior.hdr_auto`.
//! - `--legacy-hdr`  : the old HDR-only display manager tray ([`app::run`]).
//! - `--record-tray-smoke` : tray/menu check with no capture.
//!
//! The long-running modes are single-instance (a named mutex); a second launch
//! exits quietly with code 0. The diagnostic subcommands remain, all safe by
//! construction:
//! - `--probe`      : read-only NvAPI CSC reachability probe.
//! - `--clamp-test` : apply the sRGB clamp for ~3s, then reset. Prints GET state
//!   before / mid / after and guarantees a reset on every exit path.
//! - `--restore`    : reset the display to the user's journalled baseline (repairs
//!   a prior hard-kill) via the same [`hdr_exec::AppRestore`] the tray app uses.
//!
//! Design invariant: **we never leave the clamp applied on exit.** `--clamp-test`
//! resets in-line and `main` never returns while a clamp is live.
//!
//! GUI subsystem (debug builds too): the login-time Run-key launch must not open
//! a console window (closing it would kill the recorder). The diagnostic
//! subcommands re-attach to the invoking terminal's console so their log lines
//! still print there; every line also lands in `%APPDATA%\recall-hdr\recall-hdr.log`.

#![cfg_attr(not(test), windows_subsystem = "windows")]
// Wave-0 frozen contracts (`types.rs`) and `NoopClamp` are intentionally defined
// ahead of their Wave-1 consumers (the rule engine + reconciler). Allow the
// dead-code warnings until those land rather than littering per-item attributes.
#![allow(dead_code)]

mod cues;
mod app;
mod clamp;
mod config;
mod control;
mod display;
mod edid;
mod engine_policy;
mod frame_timing;
mod guard;
mod hdr;
mod hdr_exec;
mod hotkey;
mod journal;
mod liveness;
mod logging;
mod matrix;
mod overlay_proc;
mod record_actions;
mod record_activity;
mod record_app;
mod record_chord;
mod record_cmds;
mod record_config;
mod record_engine;
mod record_env;
mod record_hdr;
mod record_hotkeys;
mod record_session;
mod record_status;
mod record_notice;
mod record_storage;
mod record_target;
mod record_tray;
mod rules;
mod state;
mod supervisor;
mod tray;
mod types;
mod watcher;
mod wcs;

use std::thread::sleep;
use std::time::Duration;

use clamp::{Capability, GamutClamp, NvApiClamp};
use types::ClampMode;

/// Session-local mutex shared by every long-running mode (they all own the
/// control pipe and the hotkeys).
const INSTANCE_MUTEX: &str = "Local\\recall-hdr-controller";

fn main() {
    let arg = std::env::args().nth(1);
    let arg = arg.as_deref().unwrap_or("");
    if !matches!(arg, "" | "--record" | "--record-only" | "--legacy-hdr" | "--record-tray-smoke") {
        attach_parent_console();
    }
    logging::init();
    log!("command: {}", if arg.is_empty() { "(record mode)" } else { arg });

    let code = match arg {
        "" | "--record" | "--record-only" => single_instance(record_app::run),
        "--legacy-hdr" => single_instance(app::run),
        "--record-tray-smoke" => record_session::run_tray_smoke(),
        "--probe" => cmd_probe(),
        "--clamp-test" => cmd_clamp_test(),
        "--restore" => cmd_restore(),
        other => {
            log!("unknown arg '{other}'. usage: recall-hdr [--record|--record-only|--legacy-hdr|--record-tray-smoke|--probe|--clamp-test|--restore] (no args = record mode)");
            2
        }
    };
    std::process::exit(code);
}

/// Diagnostics print to the terminal that launched them (no-op otherwise).
fn attach_parent_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// Run `mode` unless another controller already holds the instance mutex, in
/// which case exit quietly (0).
fn single_instance(mode: fn() -> i32) -> i32 {
    match acquire_instance(INSTANCE_MUTEX) {
        Some(true) => mode(),
        Some(false) => {
            log!("another Recall controller is already running; exiting");
            0
        }
        None => {
            log!("single-instance mutex unavailable; continuing without it");
            mode()
        }
    }
}

/// Create/open the named mutex. `Some(true)` = we are the first instance (the
/// handle is leaked on purpose: it lives exactly as long as the process),
/// `Some(false)` = it already existed, `None` = the call failed.
fn acquire_instance(name: &str) -> Option<bool> {
    // Raw kernel32 import: the windows-rs CreateMutexW binding sits behind the
    // Win32_Security feature, which this crate does not enable.
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(attributes: *const std::ffi::c_void, initial_owner: i32, name: *const u16) -> *mut std::ffi::c_void;
        fn GetLastError() -> u32;
    }
    const ERROR_ALREADY_EXISTS: u32 = 183;
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
    if handle.is_null() {
        return None;
    }
    Some(unsafe { GetLastError() } != ERROR_ALREADY_EXISTS)
}

/// Read-only probe: report NvAPI reachability + current CSC state.
fn cmd_probe() -> i32 {
    log!("--- PROBE (read-only) ---");
    let (cap, summary) = clamp::probe();
    match &cap {
        Capability::Available => log!("Capability: AVAILABLE"),
        Capability::Unavailable(why) => log!("Capability: UNAVAILABLE -- {why}"),
    }
    log!("CSC GET: {summary}");
    match cap {
        Capability::Available => 0,
        Capability::Unavailable(_) => 1,
    }
}

/// Live set+revert test. Applies the sRGB clamp for ~3s then resets, printing
/// GET state before/mid/after. Aborts cleanly (no mutation) if HDR is on / -104.
fn cmd_clamp_test() -> i32 {
    log!("--- CLAMP-TEST (live set + revert) ---");

    // HDR gate: the clamp is rejected while HDR is on. Check first so we can
    // abort without ever calling SET.
    match hdr::primary_is_hdr() {
        Some(true) => {
            log!("ABORT: primary display is in HDR. The NvAPI clamp is rejected while HDR is ON (driver constraint since 531.79). Turn Windows HDR off and retry. No display state was changed.");
            return 3;
        }
        Some(false) => log!("HDR state: OFF (clamp permitted)"),
        None => log!("HDR state: UNKNOWN (DXGI probe failed); proceeding cautiously"),
    }

    let mut nv = match NvApiClamp::init() {
        Ok(n) => n,
        Err(e) => {
            log!("ABORT: NvAPI init failed: {e}");
            return 1;
        }
    };

    // BEFORE.
    let before = match nv.get_summary() {
        Ok(s) => s,
        Err(e) => {
            log!("ABORT: initial GET failed: {e}");
            return 1;
        }
    };
    log!("GET before: {before}");

    // APPLY sRGB clamp.
    if let Err(e) = nv.apply(ClampMode::Accurate) {
        // If this is the HDR -104 case that slipped past the DXGI gate, say so.
        if e.contains("-104") {
            log!("SET failed with -104 (NVAPI_NOT_SUPPORTED): this is the HDR-on rejection. Ensuring reset and aborting.");
        } else {
            log!("SET (clamp) failed: {e}");
        }
        // Guarantee we don't leave anything half-applied.
        let _ = nv.reset();
        return 1;
    }

    // MID: verify the clamp is engaged.
    let mid = nv.get_summary().unwrap_or_else(|e| format!("GET failed: {e}"));
    log!("GET mid (clamp engaged): {mid}");
    let mid_active = mid.contains("active=true");
    if !mid_active {
        log!("WARNING: mid-state GET does not show clamp active -- SET may not have taken. Resetting.");
    }

    // Hold ~3s so the user can see the desaturation.
    log!("holding clamp for 3s ...");
    sleep(Duration::from_secs(3));

    // RESET.
    if let Err(e) = nv.reset() {
        log!("ERROR: reset failed: {e}. Attempting a second reset.");
        let _ = nv.reset();
    }

    // AFTER: must equal before.
    let after = nv.get_summary().unwrap_or_else(|e| format!("GET failed: {e}"));
    log!("GET after (reset): {after}");

    // Verdict.
    let reverted = after == before || after.contains("active=false");
    if mid_active && reverted {
        log!("CLAMP-TEST PASS: engaged mid, reverted after. after==before: {}", after == before);
        0
    } else {
        log!("CLAMP-TEST INCONCLUSIVE: mid_active={mid_active} reverted={reverted}");
        1
    }
}

/// Restore the display to the user's journalled baseline. Reads the persisted
/// `state.json` and reconciles HDR / clamp / SDR-white through the SAME
/// [`hdr_exec::AppRestore`] the tray app uses, so a hard-kill left-over is repaired
/// identically whether the tray relaunches or the user runs `--restore` by hand.
///
/// A clean journal (nothing touched) is the common case and reports success with
/// nothing to do. On a partial failure the journal is left dirty for a retry.
fn cmd_restore() -> i32 {
    log!("--- RESTORE (journal-backed) ---");

    let cfg = config::Config::load_or_default();
    let mut ops = match hdr_exec::AppRestore::new(&cfg) {
        Ok(o) => o,
        Err(e) => {
            log!("RESTORE: cannot build restore ops ({e}); nothing restored.");
            // Not fatal: if we can't even resolve the display there's nothing we
            // could have safely restored. Exit 0 so a login-time `--restore` never
            // blocks boot on a transiently-absent panel.
            return 0;
        }
    };

    match journal::restore_with(&mut ops) {
        Ok(true) => {
            log!("RESTORE: journalled baseline reapplied; journal cleared.");
            0
        }
        Ok(false) => {
            log!("RESTORE: journal clean -- nothing to restore.");
            0
        }
        Err(e) => {
            log!("RESTORE: incomplete ({e}); journal left dirty for the next attempt.");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_of_the_instance_mutex_reports_already_running() {
        // A test-unique name: never the real controller's mutex.
        let name = format!("Local\\recall-hdr-test-{}", std::process::id());
        assert_eq!(acquire_instance(&name), Some(true), "first instance");
        assert_eq!(acquire_instance(&name), Some(false), "second launch sees it");
    }
}
