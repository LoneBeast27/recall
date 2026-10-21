//! The journalled HDR / clamp / SDR-white executor and the restore spine,
//! shared by the legacy HDR tray (`app.rs`) and the unified record mode's
//! optional HDR manager (`record_hdr.rs`). Moved verbatim out of `app.rs`; the
//! "display is never left stuck" rules are unchanged:
//!
//! - every mutation is journalled *before* it happens ([`Journal::record`]);
//! - one [`OnceRestore`] wrapping a real [`AppRestore`] is wired to BOTH the RAII
//!   drop-guard and the console ctrl handler;
//! - a persistently failing action is force-confirmed after
//!   [`MAX_ACTION_FAILURES`] so the reconcile loop never spins forever.

use std::sync::Arc;

use crate::clamp::{Capability, GamutClamp, NoopClamp, NvApiClamp};
use crate::config::Config;
use crate::display::{self, TargetId};
use crate::guard::{self, OnceRestore, RestoreGuard};
use crate::journal::{self, Journal, RestoreOps};
use crate::log;
use crate::state::Reconciler;
use crate::types::{Action, ClampMode};

/// After this many consecutive failures of the *same* action kind, we `confirm`
/// it anyway so the reconcile loop stops re-emitting a doomed action forever.
pub(crate) const MAX_ACTION_FAILURES: u32 = 3;

// ===========================================================================
// RestoreOps: the real display-restore executor (shared by every restore path)
// ===========================================================================

/// The concrete [`RestoreOps`] the journal drives on any restore path. Owns a
/// resolved [`TargetId`] and a gamut-clamp backend, so clean shutdown,
/// RestoreNow, the ctrl-handler, the drop-guard and `--restore` all apply an
/// identical restore.
pub(crate) struct AppRestore {
    target: TargetId,
    clamp: Box<dyn GamutClamp + Send>,
}

impl AppRestore {
    /// Build a restore executor. Resolves the managed display by friendly name and
    /// initializes the NvAPI clamp (falling back to a no-op clamp if NvAPI is
    /// unavailable -- a restore must never fail to construct just because the GPU
    /// clamp is absent; HDR/SDR-white restore is adapter-agnostic).
    pub(crate) fn new(cfg: &Config) -> Result<AppRestore, String> {
        let target = display::resolve_target(&cfg.display.target_friendly_name)?;
        let clamp = build_clamp();
        Ok(AppRestore { target, clamp })
    }
}

impl RestoreOps for AppRestore {
    fn set_hdr(&mut self, on: bool) -> Result<(), String> {
        display::set_hdr_state(&self.target, on)
    }
    fn clear_clamp(&mut self) -> Result<(), String> {
        self.clamp.reset()
    }
    fn set_sdr_white(&mut self, nits: u32) -> Result<(), String> {
        display::set_sdr_white(&self.target, nits)
    }
}

/// Construct a clamp backend: the real NvAPI clamp when reachable, else a logged
/// no-op (so the app runs on non-NVIDIA / driverless boxes without special-casing
/// every call site). `Send` so it can live in the ctrl-handler closure.
pub(crate) fn build_clamp() -> Box<dyn GamutClamp + Send> {
    match NvApiClamp::init() {
        Ok(nv) => {
            log!("app: NvAPI clamp initialized");
            Box::new(nv)
        }
        Err(e) => {
            log!("app: NvAPI clamp unavailable ({e}); using no-op clamp (HDR/SDR-white still managed)");
            Box::new(NoopClamp)
        }
    }
}

/// Build the [`AppRestore`]-backed [`OnceRestore`] and wire it to the RAII guard
/// and the console control handler. Returns the guard (to disarm on clean exit)
/// and a shared `OnceRestore` handle run explicitly on clean shutdown.
/// The ctrl handler can be installed once per process.
pub(crate) fn install_restore_spine(cfg: &Config) -> Result<(RestoreGuard, Arc<OnceRestore>), String> {
    let mut ops = AppRestore::new(cfg)?;
    let once = OnceRestore::new(Box::new(move || {
        match journal::restore_with(&mut ops) {
            Ok(true) => log!("app: restore spine applied baseline"),
            Ok(false) => log!("app: restore spine -- nothing to restore"),
            Err(e) => log!("app: restore spine INCOMPLETE ({e}); journal left dirty"),
        }
    }));
    let guard = RestoreGuard::arm_shared(once.clone());
    guard::install_shared(once.clone())?;
    Ok((guard, once))
}

/// Startup repair: a prior hard kill may have left the display dirty.
pub(crate) fn repair_if_dirty(cfg: &Config) {
    if !Journal::is_dirty() {
        return;
    }
    log!("app: journal is dirty at startup -- repairing a prior hard-kill's display state");
    match AppRestore::new(cfg) {
        Ok(mut ops) => match journal::restore_with(&mut ops) {
            Ok(true) => log!("app: startup restore complete"),
            Ok(false) => log!("app: startup restore -- nothing to restore"),
            Err(e) => log!("app: startup restore INCOMPLETE ({e}); journal left dirty for retry"),
        },
        Err(e) => log!("app: startup restore skipped -- cannot build restore ops: {e}"),
    }
}

/// Repeatable restore-to-baseline (RestoreNow / HDR management switched off):
/// the shared `OnceRestore` fires only once, so drive a fresh `AppRestore`.
pub(crate) fn restore_now(cfg: &Config) {
    match AppRestore::new(cfg) {
        Ok(mut ops) => {
            let _ = journal::restore_with(&mut ops);
        }
        Err(e) => log!("app: RestoreNow -- cannot build ops: {e}"),
    }
}

// ===========================================================================
// Per-action failure tracking (avoid infinite retry storms)
// ===========================================================================

/// Tracks consecutive failures per action *kind* so a persistently-failing action
/// is force-confirmed after [`MAX_ACTION_FAILURES`] rather than re-emitted forever.
#[derive(Default)]
pub(crate) struct FailureCounter {
    enable_hdr: u32,
    disable_hdr: u32,
    set_clamp: u32,
    clear_clamp: u32,
    set_sdr_white: u32,
}

impl FailureCounter {
    /// Bump the counter for `action`'s kind and return the new consecutive count.
    pub(crate) fn bump(&mut self, action: Action) -> u32 {
        let slot = self.slot(action);
        *slot += 1;
        *slot
    }
    /// Reset the counter for `action`'s kind (called on success).
    pub(crate) fn reset(&mut self, action: Action) {
        *self.slot(action) = 0;
    }
    fn slot(&mut self, action: Action) -> &mut u32 {
        match action {
            Action::EnableHdr => &mut self.enable_hdr,
            Action::DisableHdr => &mut self.disable_hdr,
            Action::SetClamp(_) => &mut self.set_clamp,
            Action::ClearClamp => &mut self.clear_clamp,
            Action::SetSdrWhite(_) => &mut self.set_sdr_white,
            // Noop never fails; give it a throwaway slot it never reaches.
            Action::Noop => &mut self.set_sdr_white,
        }
    }
}

// ===========================================================================
// The executor
// ===========================================================================

/// Applies reconciler [`Action`]s to the managed display, journal-before-mutate.
pub(crate) struct HdrExecutor {
    target: TargetId,
    journal: Journal,
    /// Executor clamp backend (distinct instance from the restore spine's clamp;
    /// both are cheap handles to the same driver).
    clamp: Box<dyn GamutClamp + Send>,
    failures: FailureCounter,
}

impl HdrExecutor {
    pub(crate) fn new(target: TargetId) -> Self {
        HdrExecutor { target, journal: Journal::open(), clamp: build_clamp(), failures: FailureCounter::default() }
    }

    pub(crate) fn target(&self) -> &TargetId {
        &self.target
    }

    pub(crate) fn reset_failures(&mut self) {
        self.failures = FailureCounter::default();
    }

    /// Shutdown helper: clear a journal that a completed restore left dirty.
    pub(crate) fn clear_journal(&mut self) -> Result<(), String> {
        self.journal.clear()
    }

    /// Apply one [`Action`] to the hardware with journal-before-mutate, confirming
    /// the reconciler on success (advancing `actual` so the next diff is correct).
    /// On failure we log and, after [`MAX_ACTION_FAILURES`] consecutive failures of
    /// the same action kind, confirm anyway so the loop stops spinning on a doomed
    /// action.
    pub(crate) fn execute(&mut self, action: Action, reconciler: &mut Reconciler) {
        let result = match action {
            Action::EnableHdr => self.exec_enable_hdr(),
            Action::DisableHdr => self.exec_disable_hdr(),
            Action::SetClamp(mode) => self.exec_set_clamp(mode),
            Action::ClearClamp => self.exec_clear_clamp(),
            Action::SetSdrWhite(nits) => self.exec_set_sdr_white(nits),
            Action::Noop => return,
        };

        match result {
            Ok(()) => {
                self.failures.reset(action);
                reconciler.confirm(action);
                // When we return to baseline (nothing left touched), clear the
                // journal so a later clean launch has nothing to repair.
                if !self.journal.baseline().any_touched() {
                    let _ = self.journal.clear();
                }
            }
            Err(e) => {
                let n = self.failures.bump(action);
                log!("app: action {action:?} failed ({e}) [{n}/{MAX_ACTION_FAILURES}]");
                if n >= MAX_ACTION_FAILURES {
                    log!("app: action {action:?} force-confirmed after {n} failures (breaking retry storm)");
                    self.failures.reset(action);
                    reconciler.confirm(action);
                }
            }
        }
    }

    fn exec_enable_hdr(&mut self) -> Result<(), String> {
        // Journal the ORIGINAL (pre-first-touch) HDR state before mutating.
        if !self.journal.baseline().hdr_touched {
            let original = display::get_hdr_state(&self.target).unwrap_or(false);
            self.journal.record(|b| {
                b.hdr_touched = true;
                b.hdr_original = original;
            })?;
        }
        display::set_hdr_state(&self.target, true)
    }

    fn exec_disable_hdr(&mut self) -> Result<(), String> {
        // Reconciler semantics: the desktop baseline is HDR-off, so DisableHdr sets
        // false. We still journal that we touched HDR (record original as current
        // if not already recorded) so a crash mid-transition can be repaired.
        if !self.journal.baseline().hdr_touched {
            let original = display::get_hdr_state(&self.target).unwrap_or(false);
            self.journal.record(|b| {
                b.hdr_touched = true;
                b.hdr_original = original;
            })?;
        }
        display::set_hdr_state(&self.target, false)?;

        // Returning to the HDR-OFF baseline un-owes the restore for the facets a
        // live HDR session touched -- WITHOUT this the journal is stranded dirty
        // after every clean live revert. We un-touch AFTER the HDR flip succeeds
        // (mutate-then-journal for the un-touch) so the journal never under-claims
        // a LIVE mutation.
        //
        //  * SDR-white is an HDR-MODE property; with HDR off it owes nothing. This
        //    mirrors the reconciler, whose `confirm(DisableHdr)` resets its
        //    SDR-white model to the "unset" sentinel.
        //  * HDR is un-touched ONLY when we returned it to its recorded original
        //    (`hdr_original == false`). If the user's original was HDR-ON a
        //    restore is still genuinely owed, so we KEEP `hdr_touched`.
        let base = self.journal.baseline();
        let untouch_hdr = base.hdr_touched && !base.hdr_original;
        if untouch_hdr || base.sdr_white_touched {
            self.journal.record(|b| {
                if untouch_hdr {
                    b.hdr_touched = false;
                }
                b.sdr_white_touched = false;
                b.sdr_white_original = 0;
            })?;
        }
        Ok(())
    }

    fn exec_set_clamp(&mut self, mode: ClampMode) -> Result<(), String> {
        self.journal.record(|b| b.clamp_touched = true)?;
        match self.clamp.apply(mode) {
            Ok(()) => Ok(()),
            Err(e) => {
                // A genuinely Unavailable clamp is a degraded mode, not a hard
                // failure that should block reconvergence: log + succeed.
                if let Capability::Unavailable(why) = self.clamp.capability() {
                    log!("app: SetClamp({mode:?}) unavailable ({why}); confirming anyway (degraded)");
                    return Ok(());
                }
                Err(e)
            }
        }
    }

    fn exec_clear_clamp(&mut self) -> Result<(), String> {
        // Clearing returns us toward baseline for the clamp facet.
        self.journal.record(|b| b.clamp_touched = false)?;
        self.clamp.reset()
    }

    fn exec_set_sdr_white(&mut self, nits: u32) -> Result<(), String> {
        if !self.journal.baseline().sdr_white_touched {
            let original = display::get_sdr_white(&self.target).unwrap_or(0);
            self.journal.record(|b| {
                b.sdr_white_touched = true;
                b.sdr_white_original = original;
            })?;
        }
        display::set_sdr_white(&self.target, nits)
    }
}

/// The MHC2 profile file for a `set_ac_profile` variant.
pub(crate) fn ac_profile_file(variant: &str) -> Option<&'static str> {
    match variant {
        "punchy" => Some("PG27AQDP_Punchy-Gamma2.2_MHC2.icm"),
        "accurate" => Some("PG27AQDP_sRGB-to-Gamma2.2_MHC2.icm"),
        _ => None,
    }
}

/// The `hdr` module's `ac_profile` result event, escaped by serde_json.
pub(crate) fn ac_profile_event(result: &Result<&str, String>) -> String {
    match result {
        Ok(profile) => serde_json::json!({"type": "ac_profile", "profile": profile, "ok": true}),
        Err(e) => serde_json::json!({"type": "ac_profile", "ok": false, "error": e}),
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_counter_bumps_and_resets_per_kind() {
        let mut fc = FailureCounter::default();
        assert_eq!(fc.bump(Action::EnableHdr), 1);
        assert_eq!(fc.bump(Action::EnableHdr), 2);
        // A different kind is independent.
        assert_eq!(fc.bump(Action::DisableHdr), 1);
        // SetClamp variants share one slot regardless of mode.
        assert_eq!(fc.bump(Action::SetClamp(ClampMode::Accurate)), 1);
        assert_eq!(fc.bump(Action::SetClamp(ClampMode::Punchy)), 2);
        fc.reset(Action::EnableHdr);
        assert_eq!(fc.bump(Action::EnableHdr), 1, "reset zeroed the EnableHdr slot");
        // DisableHdr untouched by the EnableHdr reset.
        assert_eq!(fc.bump(Action::DisableHdr), 2);
    }

    #[test]
    fn failure_counter_hits_threshold() {
        let mut fc = FailureCounter::default();
        let mut last = 0;
        for _ in 0..MAX_ACTION_FAILURES {
            last = fc.bump(Action::ClearClamp);
        }
        assert_eq!(last, MAX_ACTION_FAILURES, "threshold reached after N bumps");
    }

    #[test]
    fn ac_profile_event_escapes_error_text() {
        let err: Result<&str, String> = Err(r#"WCS said "no" at C:\x"#.into());
        let v: serde_json::Value = serde_json::from_str(&ac_profile_event(&err)).expect("valid JSON");
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], r#"WCS said "no" at C:\x"#);
        let ok: serde_json::Value = serde_json::from_str(&ac_profile_event(&Ok("p.icm"))).unwrap();
        assert_eq!(ok["profile"], "p.icm");
        assert_eq!(ac_profile_file("punchy"), Some("PG27AQDP_Punchy-Gamma2.2_MHC2.icm"));
        assert_eq!(ac_profile_file("vivid"), None);
    }
}
