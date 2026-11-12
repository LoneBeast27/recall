//! `recall-hdr.exe --fix-apply …` / `--fix-undo …`: the fixer's helper. The
//! app starts it (through Windows' administrator prompt only for a change that needs one) after the user clicked
//! Fix (or Undo) for one fix. It takes everything from its command line,
//! checks it again, makes that one change, writes one result file into its
//! own account's fixes folder and exits. It starts no recorder, no window and
//! no log of its own (the running recorder owns the log).

use recall_fixes::helper::{self, Outcome, Request};
use recall_fixes::FixId;

/// Exit codes: 0 done, 1 the change failed (the result file says why), 2 refused request.
pub fn run(args: &[String]) -> i32 {
    let req = match Request::parse(args) {
        Ok(r) => r,
        Err(_) => return 2,
    };
    let outcome = match &req {
        Request::Apply { fix, target, .. } => apply(*fix, target),
        Request::Undo { fix, target, undo, .. } => undo_one(*fix, target, undo).map(|()| Vec::new()),
    };
    let outcome = match outcome {
        Ok(undo) => Outcome { error: None, undo },
        Err(e) => Outcome { error: Some(e), undo: Vec::new() },
    };
    let code = if outcome.error.is_none() { 0 } else { 1 };
    match write(&req, &outcome) {
        Ok(()) => code,
        Err(_) => 1,
    }
}

fn apply(fix: FixId, target: &str) -> Result<Vec<recall_fixes::Undo>, String> {
    match fix {
        FixId::MinecraftOverlayNvidia => crate::fix_nvidia::apply_minecraft(target).map(|u| vec![u]),
    }
}

fn undo_one(fix: FixId, target: &str, undo: &recall_fixes::Undo) -> Result<(), String> {
    match fix {
        FixId::MinecraftOverlayNvidia => crate::fix_nvidia::undo_minecraft(target, undo),
    }
}

fn write(req: &Request, outcome: &Outcome) -> Result<(), String> {
    let dir = helper::results_dir().ok_or("LOCALAPPDATA is not set")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = helper::result_path(&dir, req.nonce())?;
    let body = serde_json::to_vec(outcome).map_err(|e| e.to_string())?;
    // create_new: never write through a file or link someone placed there first.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| e.to_string())?;
    f.write_all(&body).map_err(|e| e.to_string())
}
