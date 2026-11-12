//! The app side of the in-app fixer (`recall-fixes`): the offers the recorder
//! found, the fixes already applied, and the user's clicks. A click starts
//! the recorder's helper (`recall-hdr.exe --fix-apply …`) on its own thread;
//! a change that needs an administrator goes through Windows' own prompt.
//! Nothing here changes the PC without a click, and never in a test build.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use recall_fixes::offer::{Dismissed, Offer, fixes_dir};
use recall_fixes::run::{self, Result as RunResult};
use recall_fixes::{FixId, Receipt, Store};

/// Offers and receipts are re-read at most this often (they are small files).
const REFRESH: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub offers: Vec<Offer>,
    pub applied: Vec<Receipt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Apply,
    Undo,
}

/// The last click's result, kept until the app restarts or the next click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Last {
    pub fix: FixId,
    pub target: String,
    pub action: Action,
    pub result: RunResult,
}

static CACHE: Mutex<Option<(Instant, Snapshot)>> = Mutex::new(None);
static LAST: Mutex<Option<Last>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

/// Never a fix from RecallTesting, `cargo test` or an offscreen build.
fn blocked() -> bool {
    crate::TESTING || cfg!(test) || crate::offscreen::enabled()
}

/// What Settings shows; empty in test builds (they never read the real folders).
pub fn snapshot() -> Snapshot {
    if blocked() {
        return Snapshot::default();
    }
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, snap)) = cache.as_ref()
        && at.elapsed() < REFRESH
    {
        return snap.clone();
    }
    let snap = read();
    *cache = Some((Instant::now(), snap.clone()));
    snap
}

fn read() -> Snapshot {
    let applied = Store::default_dir().map(|d| Store::new(d).list()).unwrap_or_default();
    // An offer for a fix that is already applied to the same program is stale.
    let offers = Offer::default_dir()
        .map(|d| Offer::list(&d))
        .unwrap_or_default()
        .into_iter()
        .filter(|o| !applied.iter().any(|r| r.fix == o.plan.fix && r.target.eq_ignore_ascii_case(&o.target)))
        .collect();
    Snapshot { offers, applied }
}

fn invalidate() {
    *CACHE.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

pub fn running() -> bool {
    BUSY.load(Ordering::SeqCst)
}

pub fn last() -> Option<Last> {
    LAST.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Not now: remembered on disk for this fix and program, so the recorder
/// does not offer it again after a restart.
pub fn dismiss(offer: &Offer) {
    if blocked() {
        return;
    }
    if let Some(dir) = fixes_dir() {
        let _ = Dismissed::new(&dir).add(offer.plan.fix, &offer.target);
        let _ = Offer::remove(&dir.join("offers"), offer.plan.fix, &offer.target);
    }
    invalidate();
}

/// The Fix click.
pub fn apply(offer: Offer) {
    start(offer.plan.fix, offer.target.clone(), Action::Apply, move |helper| {
        let result = run::apply(&offer.plan, &offer.target, helper, false);
        if result == RunResult::Done
            && let Some(dir) = Offer::default_dir()
        {
            let _ = Offer::remove(&dir, offer.plan.fix, &offer.target);
        }
        result
    });
}

/// The Undo click.
pub fn undo(receipt: Receipt) {
    start(receipt.fix, receipt.target.clone(), Action::Undo, move |helper| run::undo(&receipt, helper, false));
}

fn start(fix: FixId, target: String, action: Action, work: impl FnOnce(&std::path::Path) -> RunResult + Send + 'static) {
    if blocked() || BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let result = match crate::services::daemon::controller_path() {
            Ok(helper) => work(&helper),
            Err(e) => RunResult::Failed(e),
        };
        crate::trace::player_log("fix", serde_json::json!({ "fix": fix.as_str(), "action": format!("{action:?}"), "outcome": outcome_word(&result) }));
        *LAST.lock().unwrap_or_else(|p| p.into_inner()) = Some(Last { fix, target, action, result });
        invalidate();
        BUSY.store(false, Ordering::SeqCst);
    });
}

/// The anonymous trace carries the outcome's kind only, never paths or error text.
fn outcome_word(r: &RunResult) -> &'static str {
    match r {
        RunResult::Done | RunResult::DoneWithNote(_) => "done",
        RunResult::Declined => "declined",
        RunResult::Failed(_) => "failed",
    }
}

/// The report text a failed fix pre-fills (the user sees and edits it before sending).
pub fn report_text(last: &Last, plan: Option<&recall_fixes::Plan>) -> String {
    let what = match last.action {
        Action::Apply => "apply",
        Action::Undo => "undo",
    };
    let mut text = format!("The fix \"{}\" ({}) failed to {what}.\n", last.fix.title(), last.fix.as_str());
    if let RunResult::Failed(e) = &last.result {
        text.push_str(&format!("Error: {e}\n"));
    }
    if let Some(plan) = plan {
        for step in &plan.steps {
            text.push_str(&format!("Step: {}\n", step.change.summary()));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_build_reads_nothing_and_applies_nothing() {
        assert_eq!(snapshot(), Snapshot::default());
        assert!(!running());
    }

    #[test]
    fn the_report_names_the_fix_the_error_and_each_step() {
        let last = Last { fix: FixId::MinecraftOverlayNvidia, target: r"C:\j\javaw.exe".into(), action: Action::Apply, result: RunResult::Failed("NvAPI_DRS_SaveSettings failed (-3)".into()) };
        let text = report_text(&last, None);
        assert!(text.contains("\"Show the overlay in Minecraft\" (minecraft-overlay-nvidia) failed to apply") && text.contains("SaveSettings failed (-3)"), "{text}");
    }

    #[test]
    fn the_trace_holds_only_the_outcome_kind() {
        assert_eq!(outcome_word(&RunResult::Failed(r"C:\Users\name\x".into())), "failed");
    }
}
