//! The app side of one click: start the helper (elevated when the plan needs
//! it), read its result, and keep or drop the receipt.

use std::path::Path;

use crate::elevate::{self, Started};
use crate::helper::{self, Outcome, Request};
use crate::plan::Plan;
use crate::receipt::{Receipt, Store};

/// What the card shows after the click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Result {
    Done,
    /// The user said No to Windows' prompt; nothing changed.
    Declined,
    /// Nothing changed, or the helper says what it could not do. The card
    /// offers "Report this" with this text.
    Failed(String),
}

/// Apply `plan` to `target`. `blocked` = never prompt (test/offscreen builds).
pub fn apply(plan: &Plan, target: &str, helper_exe: &Path, store: &Store, blocked: bool) -> Result {
    let req = Request::Apply { fix: plan.fix, nonce: helper::new_nonce(), target: target.to_owned(), plan: plan.clone() };
    match call(&req, helper_exe, plan.needs_admin(), blocked) {
        Ok(outcome) => {
            if let Some(e) = outcome.error {
                return Result::Failed(e);
            }
            let receipt = Receipt { fix: plan.fix, target: target.to_owned(), applied_unix: now(), plan: plan.clone(), undo: outcome.undo };
            match store.save(&receipt) {
                Ok(_) => Result::Done,
                // Applied, but Undo would not know how: say so plainly.
                Err(e) => Result::Failed(format!("the fix was applied, but its undo receipt could not be saved: {e}")),
            }
        }
        Err(r) => r,
    }
}

/// Undo `receipt`, last change first. The receipt stays until every change is undone.
pub fn undo(receipt: &Receipt, helper_exe: &Path, store: &Store, blocked: bool) -> Result {
    for u in receipt.undo.iter().rev() {
        let req = Request::Undo { fix: receipt.fix, nonce: helper::new_nonce(), target: receipt.target.clone(), undo: u.clone() };
        match call(&req, helper_exe, receipt.plan.needs_admin(), blocked) {
            Ok(Outcome { error: None, .. }) => {}
            Ok(Outcome { error: Some(e), .. }) => return Result::Failed(e),
            Err(r) => return r,
        }
    }
    match store.remove(receipt.fix, &receipt.target) {
        Ok(()) => Result::Done,
        Err(e) => Result::Failed(format!("undone, but the receipt could not be removed: {e}")),
    }
}

fn call(req: &Request, helper_exe: &Path, admin: bool, blocked: bool) -> std::result::Result<Outcome, Result> {
    let params = req.arguments().map_err(Result::Failed)?;
    let dir = helper::results_dir().ok_or_else(|| Result::Failed("LOCALAPPDATA is not set".into()))?;
    let path = helper::result_path(&dir, req.nonce()).map_err(Result::Failed)?;
    let code = match elevate::run(helper_exe, &params, admin, blocked) {
        Started::Exited(code) => code,
        Started::Declined => return Err(Result::Declined),
        Started::Blocked => return Err(Result::Failed("fixes are never applied from a test build".into())),
        Started::Failed(e) => return Err(Result::Failed(format!("Windows could not start Recall's fix helper (error {e})"))),
    };
    let body = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    match body {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| Result::Failed(format!("the helper's result is unreadable: {e}"))),
        // The helper writes into its own account's folder: a different
        // administrator account at the prompt leaves nothing here.
        Err(_) => Err(Result::Failed(format!("the helper exited with code {code} and left no result (was a different Windows account used at the prompt?)"))),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
