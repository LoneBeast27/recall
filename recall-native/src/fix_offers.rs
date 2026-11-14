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
        return preview();
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

/// Layout pictures: an offscreen app with `RECALL_FIXES_PREVIEW` = a folder holding
/// `offers\` and `applied\` shows those (read only). Clicks stay blocked; the real
/// folders are never read from a test, RecallTesting or offscreen build.
fn preview() -> Snapshot {
    let Some(dir) = std::env::var_os("RECALL_FIXES_PREVIEW").filter(|_| crate::offscreen::enabled() && !cfg!(test)) else { return Snapshot::default() };
    let dir = std::path::PathBuf::from(dir);
    Snapshot { offers: Offer::list(&dir.join("offers")), applied: Store::new(dir.join("applied")).list() }
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
    Snapshot { offers: offered_only(offers), applied }
}

/// Offers for fixes Recall still offers: a file left from before a fix was turned off shows
/// no notice. Applied fixes stay listed, so Undo still works.
fn offered_only(offers: Vec<Offer>) -> Vec<Offer> {
    offers.into_iter().filter(|o| o.plan.fix.offered()).collect()
}

fn invalidate() {
    *CACHE.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// The notice's close button after a result: the result line goes (the receipt stays).
pub fn clear_last() {
    *LAST.lock().unwrap_or_else(|p| p.into_inner()) = None;
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
    fn an_offer_left_for_a_fix_that_is_off_shows_nothing() {
        let java = "C:/Games/x/jre25/bin/javaw.exe";
        let json = format!(r#"{{"plan":{{"fix":"MinecraftOverlayNvidia","steps":[{{"change":{{"DriverAppSetting":{{"vendor":"Nvidia","exe":"{java}","entry":"x/jre25/bin/javaw.exe","setting_id":550932728,"setting_name":"Vulkan/OpenGL present method","new_profile":"Recall: Minecraft 1a2b3c4d","to":[1,"Prefer layered on DXGI Swapchain"]}}}}}}]}},"target":"{java}","seen_unix":1794500000}}"#);
        let offer: Offer = serde_json::from_str(&json).expect("an offer file");
        assert!(offered_only(vec![offer]).is_empty(), "no notice for a fix that is off");
    }

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
