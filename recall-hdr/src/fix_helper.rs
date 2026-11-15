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

/// The FPS permission fix is applied by the app through Recall Setup's elevated mode.
const APP_ONLY: &str = "this fix is applied by the Recall app, not the helper";

/// The plan the user read must still be the plan the live state gives.
fn apply(fix: FixId, target: &str, shown: &Plan, store: &Store) -> Result<Option<String>, String> {
    const CHANGED: &str = "The settings changed after Recall showed you this fix. Nothing was changed; open the fix again to see what it would do now.";
    if !may_run(fix, true) {
        return Err("Recall no longer offers this fix. Nothing was changed.".into());
    }
    match fix {
        FixId::MinecraftOverlayNvidia => match crate::fix_nvidia::minecraft_plan(target)? {
            Check::AlreadyFixed => return Err("Already fixed: Recall's profile has this setting now. Nothing was changed.".into()),
            Check::OtherProfile(name) => return Err(format!("The driver now matches this program to the profile \"{name}\". Recall does not edit existing profiles, so nothing was changed.")),
            Check::Offer(live) if live != *shown => return Err(CHANGED.into()),
            Check::Offer(_) => {}
        },
        FixId::FpsPermission => return Err(APP_ONLY.into()),
        FixId::MinecraftBorderlessCwb => match crate::fix_cwb::plan(target)? {
            crate::fix_cwb::Check::AlreadyFixed => return Err("Already fixed: the mod is set to Windows' borderless now. Nothing was changed.".into()),
            crate::fix_cwb::Check::NotApplicable(why) => return Err(format!("{why}. Nothing was changed.")),
            crate::fix_cwb::Check::Offer(live) if live != *shown => return Err(CHANGED.into()),
            crate::fix_cwb::Check::Offer(_) => {}
        },
        FixId::RocketLeagueBorderless => match crate::fix_rl::plan(target)? {
            crate::fix_rl::Check::AlreadyFixed => return Err("Already fixed: Rocket League is set to Borderless now. Nothing was changed.".into()),
            crate::fix_rl::Check::NotApplicable(why) => return Err(format!("{why}. Nothing was changed.")),
            crate::fix_rl::Check::Offer(live) if live != *shown => return Err(CHANGED.into()),
            crate::fix_rl::Check::Offer(_) => {}
        },
    }
    let undo = match fix {
        FixId::MinecraftOverlayNvidia => vec![crate::fix_nvidia::apply_minecraft(target)?],
        FixId::MinecraftBorderlessCwb => vec![crate::fix_cwb::apply(target)?],
        FixId::RocketLeagueBorderless => crate::fix_rl::apply(target)?,
        FixId::FpsPermission => return Err(APP_ONLY.into()),
    };
    let receipt = Receipt { fix, target: target.to_owned(), applied_unix: now(), plan: shown.clone(), undo };
    match store.save(&receipt) {
        Ok(_) => Ok(None),
        // No receipt means no Undo button: take the change back now.
        Err(e) => {
            // Last to first, as Undo runs.
            let back = receipt.undo.iter().rev().try_for_each(|u| undo_change(fix, target, u).map(|_| ()));
            let by_hand = match fix {
                FixId::MinecraftOverlayNvidia => format!("Remove the driver profile \"{}\" in the NVIDIA Control Panel.", fix.own_profile(target)),
                FixId::MinecraftBorderlessCwb => format!("Set \"{}\" back in {target}.", crate::fix_cwb::KEY),
                FixId::FpsPermission => String::new(),
                FixId::RocketLeagueBorderless => "Set the display mode back to Fullscreen in Rocket League's Video settings.".to_owned(),
            };
            Err(match back {
                Ok(_) => format!("Its undo record could not be saved ({e}), so Recall took the change back. Nothing is changed now."),
                Err(u) => format!("Its undo record could not be saved ({e}), and taking the change back failed too ({u}). {by_hand}"),
            })
        }
    }
}

/// Ok(Some(note)): undone, but the user's own additions were kept.
fn undo_one(receipt: &Receipt, undo: &Undo, store: &Store) -> Result<Option<String>, String> {
    let note = undo_change(receipt.fix, &receipt.target, undo)?;
    // The last change of the receipt undone: the receipt goes.
    if receipt.undo.first() == Some(undo) {
        store.remove(receipt.fix, &receipt.target)?;
    }
    Ok(note)
}

fn undo_change(fix: FixId, target: &str, undo: &Undo) -> Result<Option<String>, String> {
    match fix {
        FixId::MinecraftOverlayNvidia => crate::fix_nvidia::undo_minecraft(target, undo),
        FixId::MinecraftBorderlessCwb => crate::fix_cwb::undo(target, undo),
        FixId::FpsPermission => Err(APP_ONLY.into()),
        FixId::RocketLeagueBorderless => crate::fix_rl::undo(target, undo),
    }
}

fn write(outcome: &Outcome) -> Result<(), String> {
    let dir = helper::results_dir().ok_or("LOCALAPPDATA is not set")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = helper::result_path(&dir, &outcome.nonce)?;
    let body = serde_json::to_vec(outcome).map_err(|e| e.to_string())?;
    // Atomic: a new temp file (create_new: never through a file or link
    // placed there first), flushed, then renamed onto the name the app reads.
    let tmp = path.with_extension("json.tmp");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e| e.to_string())?;
    f.write_all(&body).and_then(|()| f.sync_all()).map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A fix Recall no longer offers is never applied (an old window or offer file); its Undo
/// always runs, so a fix applied before can be taken back.
fn may_run(fix: FixId, apply: bool) -> bool {
    !apply || fix.offered()
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    #[test]
    fn a_fix_that_is_off_is_never_applied_but_always_undone() {
        for fix in [FixId::MinecraftOverlayNvidia, FixId::MinecraftBorderlessCwb] {
            assert!(!may_run(fix, true), "{fix:?}: apply refused");
            assert!(may_run(fix, false), "{fix:?}: undo runs");
        }
        // The FPS fix is applied by the app through Setup; the helper refuses it either way.
        let plan = recall_fixes::plan::fps_permission("S-1-5-21-1-2-3-1001");
        let store = Store::new(std::env::temp_dir().join(format!("recall-helper-fps-{}", std::process::id())));
        assert_eq!(apply(FixId::FpsPermission, r"C:\x", &plan, &store).unwrap_err(), APP_ONLY);
    }
}
