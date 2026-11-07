//! recall-hdr: Recall's controller (Windows, RTX 5090).
//!
//! Entry point. Modes:
//! - (no args), `--record`, `--record-only` : the unified record mode
//!   ([`record_app::run`]) -- records any game automatically, tray, hotkeys,
//!   control pipe, overlay; HDR management only with `behavior.hdr_auto`, and
//!   only while the shelved HDR feature is re-enabled ([`hdr_gate`]).
//! - `--legacy-hdr`  : the old HDR-only display manager tray ([`app::run`]); shelved
//!   with `--clamp-test` unless `RECALL_EXPERIMENTAL_HDR=1` ([`hdr_gate`]).
//! - `--record-tray-smoke` : tray/menu check with no capture.
//!
//! The long-running modes are single-instance (a named mutex, `single_instance`); a
//! second launch never records: with no arguments it opens or focuses the Recall
//! window, with `--record` it waits up to 5 s for a quitting controller, then exits
//! with code 0. The diagnostic subcommands remain, all safe by
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
mod durable;
mod edid;
mod engine_policy;
mod frame_timing;
mod guard;
mod hdr;
mod hdr_exec;
#[path = "../../recall/src/image_path.rs"]
mod image_path;
mod hdr_gate;
mod hotkey;
mod journal;
mod known_games;
mod liveness;
mod logging;
mod matrix;
mod overlay_proc;
mod private_app_stems;
mod private_apps;
mod record_actions;
mod record_activity;
mod record_app;
mod record_cmds;
mod record_composed;
mod record_capture_watch;
mod record_app_audio;
mod record_audio_sources;
mod record_config;
mod record_detect;
mod record_visibility;
mod record_dispatch;
mod record_drain;
mod record_engine;
mod record_engine_ctl;
mod record_env;
mod record_editor_ask;
mod record_fullscreen;
mod record_hdr;
mod record_hosted;
mod record_hotkeys;
mod record_recover_stale;
mod record_screen;
mod record_session;
mod record_status;
mod record_notice;
mod record_power;
mod record_power_win;
mod record_quit;
mod record_recover;
mod record_restart;
mod record_restart_host;
mod record_render;
mod record_startup;
mod record_storage;
mod record_free_space;
mod record_janitor;
mod record_monitor;
mod record_target;
mod record_target_gate;
mod record_transition;
mod record_tray;
mod render_probe;
mod render_verdict;
mod ghost_window;
mod gpu_games;
mod rules;
#[path = "../../recall/src/single_instance.rs"]
mod single_instance;
// Volume + free-space calls shared with the engine (spill-only helpers unused here).
#[allow(dead_code)]
#[path = "../../recall/src/win_volume.rs"]
mod win_volume;
// Can a clip really go to the Recycle Bin (shared with the app's library delete).
#[allow(dead_code)]
#[path = "../../recall/src/win_recycle.rs"]
mod win_recycle;
// Recycle through IFileOperation with a sink that refuses a permanent delete (shared with the app).
#[path = "../../recall/src/win_fileop.rs"]
mod win_fileop;
mod state;
mod supervisor;
mod tray;
#[cfg(test)]
mod transitions_detect_tests;
#[cfg(test)]
mod transitions_gate_tests;
#[cfg(test)]
mod transitions_scope_tests;
#[cfg(test)]
mod transitions_restart_tests;
#[cfg(test)]
mod transitions_planned_tests;
#[cfg(test)]
mod transitions_ghost_tests;
#[cfg(test)]
mod test_src;
#[cfg(test)]
mod transitions_sim;
#[cfg(test)]
mod transitions_tests;
mod types;
mod watcher;
mod wcs;

use std::thread::sleep;
use std::time::Duration;

use clamp::{Capability, GamutClamp, NvApiClamp};
use types::ClampMode;

/// How long a scripted start (`--record`, the Run key, an install) waits for a controller
/// that is still quitting before it gives up.
const RESTART_WAIT: Duration = Duration::from_secs(5);

const USAGE: &str = "usage: recall-hdr [--record|--record-only|--legacy-hdr|--record-tray-smoke|--probe|--clamp-test|--restore|--help] (no args = record mode; a second start opens the Recall window)";

fn main() {
    let arg = std::env::args().nth(1);
    let arg = arg.as_deref().unwrap_or("");
    if !matches!(arg, "" | "--record" | "--record-only" | "--legacy-hdr" | "--record-tray-smoke") {
        attach_parent_console();
    }
    if matches!(arg, "--help" | "-h" | "/?") {
        println!("{USAGE}");
        std::process::exit(0);
    }
    logging::init();
    log!("command: {}", if arg.is_empty() { "(record mode)" } else { arg });

    let code = match arg {
        "" => single_instance(record_app::run, Launch::ByHand),
        "--record" | "--record-only" => single_instance(record_app::run, Launch::Scripted),
        "--legacy-hdr" | "--clamp-test" if !hdr_gate::enabled() => {
            log!("{arg}: {}", hdr_gate::SHELVED_MSG);
            eprintln!("{}", hdr_gate::SHELVED_MSG);
            2
        }
        "--legacy-hdr" => single_instance(app::run, Launch::Scripted),
        "--record-tray-smoke" => record_session::run_tray_smoke(),
        "--probe" => cmd_probe(),
        "--clamp-test" => cmd_clamp_test(),
        "--restore" => cmd_restore(),
        other => {
            log!("unknown arg '{other}'. {USAGE}");
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

/// How this controller was started, which decides what a second start does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Launch {
    /// No arguments: a person opened the program. A second start shows the Recall window.
    ByHand,
    /// `--record` and friends: the Run key, the app, an install. A second start waits briefly
    /// for a controller that is quitting, then exits quietly.
    Scripted,
}

/// What to do after the claim.
#[derive(Debug, PartialEq)]
enum Next {
    Run,
    RunUnguarded,
    OpenWindowAndExit,
    Exit,
}

fn next_step(claim: &single_instance::Claim, launch: Launch) -> Next {
    match (claim, launch) {
        (single_instance::Claim::First(_), _) => Next::Run,
        (single_instance::Claim::Unavailable(_), _) => Next::RunUnguarded,
        (single_instance::Claim::Taken, Launch::ByHand) => Next::OpenWindowAndExit,
        (single_instance::Claim::Taken, Launch::Scripted) => Next::Exit,
    }
}

/// Runs `mode` as the one controller. A second controller never starts a recorder: started by
/// hand it opens (or focuses) the Recall window and exits 0; started by a script it exits 0.
fn single_instance(mode: fn() -> i32, launch: Launch) -> i32 {
    let name = single_instance::mutex_name(single_instance::CONTROLLER);
    let claim = match launch {
        Launch::ByHand => single_instance::claim(&name),
        Launch::Scripted => single_instance::claim_within(&name, RESTART_WAIT),
    };
    match next_step(&claim, launch) {
        // `claim` holds the guard until the process exits.
        Next::Run => mode(),
        Next::RunUnguarded => {
            if let single_instance::Claim::Unavailable(code) = claim {
                log!("single-instance mutex unavailable (error {code}); continuing without it");
            }
            mode()
        }
        Next::OpenWindowAndExit => {
            let shown = record_tray::RecallUi::from_current_exe().open();
            if shown.is_empty() {
                log!("another Recall controller is already running; started the Recall window; exiting");
            } else {
                log!("another Recall controller is already running; brought the Recall window to the front; exiting");
            }
            0
        }
        Next::Exit => {
            log!("another Recall controller is already running; exiting");
            0
        }
    }
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
    fn a_second_controller_never_runs_a_recorder() {
        use single_instance::Claim;
        // A test-unique name: never the real controller's mutex.
        let name = format!("Local\\recall-hdr-test-{}", std::process::id());
        let first = single_instance::claim(&name);
        assert_eq!(next_step(&first, Launch::Scripted), Next::Run, "first instance records");
        let second = single_instance::claim(&name);
        assert_eq!(next_step(&second, Launch::ByHand), Next::OpenWindowAndExit, "by hand: show the window");
        assert_eq!(next_step(&second, Launch::Scripted), Next::Exit, "scripted: exit quietly");
        assert_eq!(next_step(&Claim::Unavailable(5), Launch::Scripted), Next::RunUnguarded);
        drop(first);
        assert_eq!(next_step(&single_instance::claim(&name), Launch::ByHand), Next::Run, "a crashed or quit controller never blocks");
    }

    #[test]
    fn the_controller_keeps_the_name_older_builds_hold() {
        // An install replaces a running older controller: both must see the same mutex.
        assert_eq!(single_instance::CONTROLLER, "recall-hdr-controller");
    }
}
