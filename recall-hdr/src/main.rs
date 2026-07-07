//! recall-hdr: HDR display-manager companion for Recall (Windows, RTX 5090).
//!
//! Wave 0/1 CLI entry point. Three commands, all safe by construction:
//! - `--probe`      (default): read-only NvAPI CSC reachability probe.
//! - `--clamp-test`: apply the sRGB clamp for ~3s, then reset. Prints GET state
//!   before / mid / after and guarantees a reset on every exit path.
//! - `--restore`   : (stub) will reset the display to the user's baseline.
//!
//! Design invariant: **we never leave the clamp applied on exit.** `--clamp-test`
//! resets in-line and `main` never returns while a clamp is live.

// Wave-0 frozen contracts (`types.rs`) and `NoopClamp` are intentionally defined
// ahead of their Wave-1 consumers (the rule engine + reconciler). Allow the
// dead-code warnings until those land rather than littering per-item attributes.
#![allow(dead_code)]

mod clamp;
mod config;
mod display;
mod edid;
mod guard;
mod hdr;
mod journal;
mod logging;
mod matrix;
mod rules;
mod state;
mod supervisor;
mod types;
mod watcher;

use std::thread::sleep;
use std::time::Duration;

use clamp::{Capability, GamutClamp, NvApiClamp};
use types::ClampMode;

fn main() {
    logging::init();

    let arg = std::env::args().nth(1).unwrap_or_else(|| "--probe".into());
    log!("command: {arg}");

    let code = match arg.as_str() {
        "--probe" => cmd_probe(),
        "--clamp-test" => cmd_clamp_test(),
        "--restore" => cmd_restore(),
        other => {
            log!("unknown arg '{other}'. usage: recall-hdr [--probe|--clamp-test|--restore]");
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

/// Restore the display to the user's baseline. Wave 1 will read the persisted
/// baseline and reconcile; for now it just clears any active clamp so the stub
/// is genuinely safe to run.
fn cmd_restore() -> i32 {
    log!("--- RESTORE (stub) ---");
    log!("TODO(Wave1): read persisted baseline + reconcile HDR/clamp/sdr-white.");
    match NvApiClamp::init() {
        Ok(mut nv) => {
            if let Err(e) = nv.reset() {
                log!("baseline clamp reset failed: {e}");
                return 1;
            }
            log!("cleared any active gamut clamp (native gamut restored).");
            0
        }
        Err(e) => {
            log!("NvAPI unavailable ({e}); nothing to restore.");
            0
        }
    }
}
