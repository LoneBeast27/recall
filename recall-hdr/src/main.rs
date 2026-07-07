//! recall-hdr: HDR display-manager companion for Recall (Windows, RTX 5090).
//!
//! Entry point. The default (no args) launches the tray application
//! ([`app::run`]); the diagnostic subcommands remain, all safe by construction:
//! - (no args)      : run the tray app -- auto HDR/SDR-by-game + clamp + hotkey.
//! - `--probe`      : read-only NvAPI CSC reachability probe.
//! - `--clamp-test` : apply the sRGB clamp for ~3s, then reset. Prints GET state
//!   before / mid / after and guarantees a reset on every exit path.
//! - `--restore`    : reset the display to the user's journalled baseline (repairs
//!   a prior hard-kill) via the same [`app::AppRestore`] the tray app uses.
//!
//! Design invariant: **we never leave the clamp applied on exit.** `--clamp-test`
//! resets in-line and `main` never returns while a clamp is live.

// Wave-0 frozen contracts (`types.rs`) and `NoopClamp` are intentionally defined
// ahead of their Wave-1 consumers (the rule engine + reconciler). Allow the
// dead-code warnings until those land rather than littering per-item attributes.
#![allow(dead_code)]

mod app;
mod clamp;
mod config;
mod display;
mod edid;
mod guard;
mod hdr;
mod hotkey;
mod journal;
mod logging;
mod matrix;
mod rules;
mod state;
mod supervisor;
mod tray;
mod types;
mod watcher;

use std::thread::sleep;
use std::time::Duration;

use clamp::{Capability, GamutClamp, NvApiClamp};
use types::ClampMode;

fn main() {
    logging::init();

    // Default (no args) launches the tray application. The subcommands are
    // explicit diagnostics / one-shots.
    let arg = std::env::args().nth(1);
    let arg = arg.as_deref().unwrap_or("");
    log!("command: {}", if arg.is_empty() { "(run tray app)" } else { arg });

    let code = match arg {
        "" => app::run(),
        "--probe" => cmd_probe(),
        "--clamp-test" => cmd_clamp_test(),
        "--restore" => cmd_restore(),
        other => {
            log!("unknown arg '{other}'. usage: recall-hdr [--probe|--clamp-test|--restore] (no args = run tray app)");
            2
        }
    };
    std::process::exit(code);
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
/// [`app::AppRestore`] the tray app uses, so a hard-kill left-over is repaired
/// identically whether the tray relaunches or the user runs `--restore` by hand.
///
/// A clean journal (nothing touched) is the common case and reports success with
/// nothing to do. On a partial failure the journal is left dirty for a retry.
fn cmd_restore() -> i32 {
    log!("--- RESTORE (journal-backed) ---");

    let cfg = config::Config::load_or_default();
    let mut ops = match app::AppRestore::new(&cfg) {
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
