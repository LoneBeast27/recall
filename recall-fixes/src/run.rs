//! The app side of one click: start the helper (through Windows'
//! administrator prompt only when the plan needs it) and read its result.
//! The helper itself writes and removes receipts.

use std::path::Path;

use crate::elevate::{self, Started};
use crate::helper::{self, Outcome, Request};
use crate::plan::Plan;
use crate::receipt::Receipt;

/// What the card shows after the click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Result {
    Done,
    /// Done, and the helper says what it left as the user had it.
    DoneWithNote(String),
    /// The user said No to Windows' prompt; nothing changed.
    Declined,
    /// What the helper could not do, or why it did not run. The card offers
    /// "Report this" with this text.
    Failed(String),
}

/// Apply `plan` to `target`. `blocked` = never start anything (test/offscreen builds).
pub fn apply(plan: &Plan, target: &str, helper_exe: &Path, blocked: bool) -> Result {
    let req = Request::Apply { fix: plan.fix, nonce: helper::new_nonce(), target: target.to_owned(), plan: plan.clone() };
    finish(call(&req, &plan.hash(), helper_exe, plan.needs_admin(), blocked))
}

/// Undo `receipt`, last change first. The helper removes the receipt once all are undone.
pub fn undo(receipt: &Receipt, helper_exe: &Path, blocked: bool) -> Result {
    for u in receipt.undo.iter().rev() {
        let req = Request::Undo { fix: receipt.fix, nonce: helper::new_nonce(), target: receipt.target.clone(), undo: u.clone() };
        match finish(call(&req, &receipt.plan.hash(), helper_exe, receipt.plan.needs_admin(), blocked)) {
            Result::Done => {}
            other => return other,
        }
    }
    Result::Done
}

fn finish(r: std::result::Result<Outcome, Result>) -> Result {
    match r {
        Ok(Outcome { error: None, note: Some(n), .. }) => Result::DoneWithNote(n),
        Ok(Outcome { error: None, .. }) => Result::Done,
        Ok(Outcome { error: Some(e), .. }) => Result::Failed(e),
        Err(r) => r,
    }
}

fn call(req: &Request, plan_hash: &str, helper_exe: &Path, admin: bool, blocked: bool) -> std::result::Result<Outcome, Result> {
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
    let outcome: Outcome = match body {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| Result::Failed(format!("the helper's result is unreadable: {e}")))?,
        // A different administrator account at the prompt writes to its own folder;
        // a helper still running writes its receipt itself when it finishes.
        Err(_) => return Err(Result::Failed(format!("the helper exited with code {code} and left no result; check Settings again in a moment"))),
    };
    if outcome.nonce != req.nonce() || outcome.plan_hash != plan_hash {
        return Err(Result::Failed("the helper's result belongs to another request".into()));
    }
    Ok(outcome)
}
