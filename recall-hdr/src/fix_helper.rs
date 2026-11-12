//! `recall-hdr.exe --fix-apply …` / `--fix-undo …`: the fixer's helper. The
//! app starts it after the user clicked Fix (or Undo) for one fix, through
//! Windows' administrator prompt only for a change that needs one. It takes
//! everything from its command line and checks it again, makes that one
//! change, writes (or removes) the receipt itself, then writes one result
//! file tied to the request into its own account's fixes folder and exits.
//! It starts no recorder, no window and no log of its own.

use recall_fixes::helper::{self, Outcome, Request};
use recall_fixes::{FixId, Plan, Receipt, Store, Undo};

use crate::fix_nvidia::Check;

/// Exit codes: 0 done, 1 the change failed (the result file says why), 2 refused request.
pub fn run(args: &[String]) -> i32 {
    let req = match Request::parse(args) {
        Ok(r) => r,
        Err(_) => return 2,
    };
    let Some(store) = Store::default_dir().map(Store::new) else { return 1 };
    let (plan_hash, result) = match &req {
        Request::Apply { fix, target, plan, .. } => (plan.hash(), apply(*fix, target, plan, &store)),
        Request::Undo { fix, target, undo, .. } => match store.find(*fix, target) {
            // Only a change Recall's own receipt lists may be undone.
            Some(r) if r.undo.contains(undo) => (r.plan.hash(), undo_one(&r, undo, &store)),
            _ => (String::new(), Err("no receipt lists this change; nothing was changed".into())),
        },
    };
    let (note, error) = match result {
        Ok(note) => (note, None),
        Err(e) => (None, Some(e)),
    };
    let outcome = Outcome { nonce: req.nonce().to_owned(), plan_hash, error, note };
    let code = if outcome.error.is_none() { 0 } else { 1 };
    match write(&outcome) {
        Ok(()) => code,
        Err(_) => 1,
    }
}

/// The plan the user read must still be the plan the live state gives.
fn apply(fix: FixId, target: &str, shown: &Plan, store: &Store) -> Result<Option<String>, String> {
    let live = match fix {
        FixId::MinecraftOverlayNvidia => crate::fix_nvidia::minecraft_plan(target)?,
    };
    match live {
        Check::AlreadyFixed => return Err("Already fixed: Recall's profile has this setting now. Nothing was changed.".into()),
        Check::OtherProfile(name) => return Err(format!("The driver now matches this program to the profile \"{name}\". Recall does not edit existing profiles, so nothing was changed.")),
        Check::Offer(live) if live != *shown => return Err("The driver settings changed after Recall showed you this fix. Nothing was changed; open the fix again to see what it would do now.".into()),
        Check::Offer(_) => {}
    }
    let undo = match fix {
        FixId::MinecraftOverlayNvidia => crate::fix_nvidia::apply_minecraft(target)?,
    };
    let receipt = Receipt { fix, target: target.to_owned(), applied_unix: now(), plan: shown.clone(), undo: vec![undo] };
    store.save(&receipt).map(|_| None).map_err(|e| format!("the fix was applied, but its undo receipt could not be saved: {e}"))
}

/// Ok(Some(note)): undone, but the user's own additions were kept.
fn undo_one(receipt: &Receipt, undo: &Undo, store: &Store) -> Result<Option<String>, String> {
    let note = match receipt.fix {
        FixId::MinecraftOverlayNvidia => crate::fix_nvidia::undo_minecraft(&receipt.target, undo)?,
    };
    // The last change of the receipt undone: the receipt goes.
    if receipt.undo.first() == Some(undo) {
        store.remove(receipt.fix, &receipt.target)?;
    }
    Ok(note)
}

fn write(outcome: &Outcome) -> Result<(), String> {
    let dir = helper::results_dir().ok_or("LOCALAPPDATA is not set")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = helper::result_path(&dir, &outcome.nonce)?;
    let body = serde_json::to_vec(outcome).map_err(|e| e.to_string())?;
    // create_new: never write through a file or link someone placed there first.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| e.to_string())?;
    f.write_all(&body).map_err(|e| e.to_string())
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
