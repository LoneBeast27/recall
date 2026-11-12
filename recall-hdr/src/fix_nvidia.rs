//! The NVIDIA side of the fixer: the Minecraft overlay fix's plan (read-only)
//! and its helper actions (the writes). Kept apart so AMD and Intel can sit
//! beside it.
//!
//! Recall never edits an existing driver profile: the fix is offered only
//! when the driver matches the program to no profile at all, and it then
//! makes Recall's own profile with one full-path entry.

use std::path::Path;

use recall_fixes::plan::{minecraft_overlay_nvidia, DriverAppSetting, Plan, Vendor};
use recall_fixes::{FixId, Undo};
use recall_nvidia::{present_method as pm, DriverWriter, Placement, Reverted};

const FIX: FixId = FixId::MinecraftOverlayNvidia;

/// What the read-only check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Offer(Plan),
    /// Recall's own profile already sets it.
    AlreadyFixed,
    /// Another profile holds the program (possibly by file name): Recall
    /// leaves it alone, so no offer.
    OtherProfile(String),
}

/// A Java runtime: the only programs this fix may touch.
pub fn is_java(target: &str) -> bool {
    Path::new(target).file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case("javaw.exe") || n.eq_ignore_ascii_case("java.exe"))
}

fn check_target(target: &str) -> Result<(), String> {
    if !is_java(target) {
        return Err("the program is not a Java runtime".into());
    }
    if !Path::new(target).is_file() {
        return Err("the program no longer exists at its path".into());
    }
    Ok(())
}

/// A path beside `exe` that no program has: if the driver matches it to
/// Recall's new profile, the entry is keyed by file name, not by path.
fn probe_path(exe: &str) -> String {
    let p = Path::new(exe);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("javaw.exe");
    let dir = p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
    format!("{dir}\\recall-path-check\\{name}")
}

/// Read-only: the plan for `exe`, or why there is none.
pub fn minecraft_plan(exe: &str) -> Result<Check, String> {
    check_target(exe)?;
    let own = FIX.own_profile(exe);
    match DriverWriter::load()?.placement(exe, pm::ID)? {
        Placement::Profile { name, value } if name == own && value == Some(pm::LAYERED_ON_DXGI) => Ok(Check::AlreadyFixed),
        Placement::Profile { name, .. } => Ok(Check::OtherProfile(name)),
        Placement::None => Ok(Check::Offer(minecraft_overlay_nvidia(DriverAppSetting {
            vendor: Vendor::Nvidia,
            exe: exe.to_owned(),
            setting_id: pm::ID,
            setting_name: pm::NAME.to_owned(),
            new_profile: own,
            to: (pm::LAYERED_ON_DXGI, pm::value_name(pm::LAYERED_ON_DXGI)),
        }))),
    }
}

/// Helper: make Recall's own profile for `target`.
pub fn apply_minecraft(target: &str) -> Result<Undo, String> {
    check_target(target)?;
    let profile = FIX.own_profile(target);
    DriverWriter::load()?.create_own(target, &probe_path(target), pm::ID, pm::LAYERED_ON_DXGI, &profile)?;
    Ok(Undo::DriverOwnProfile { vendor: Vendor::Nvidia, exe: target.to_owned(), setting_id: pm::ID, value: pm::LAYERED_ON_DXGI, profile })
}

/// Helper: undo one receipt entry, after checking it could only have come
/// from [`apply_minecraft`] for this same program. Returns a note for the
/// user when only part of the profile was Recall's.
pub fn undo_minecraft(target: &str, undo: &Undo) -> Result<Option<String>, String> {
    let Undo::DriverOwnProfile { vendor: Vendor::Nvidia, exe, setting_id, value, profile } = undo;
    if !exe.eq_ignore_ascii_case(target) || *setting_id != pm::ID || *value != pm::LAYERED_ON_DXGI || !is_java(target) || *profile != FIX.own_profile(target) {
        return Err("the undo receipt does not match this fix".into());
    }
    Ok(match DriverWriter::load()?.revert_own(target, pm::ID, pm::LAYERED_ON_DXGI, profile)? {
        Reverted::ProfileRemoved | Reverted::AlreadyGone => None,
        Reverted::Partly { setting_kept, entry_kept } => Some(format!(
            "You added to \"{profile}\" since, so it stays. {} {}",
            if setting_kept { "Its present method was changed after the fix, so it was left as you set it." } else { "Recall's present method setting was removed." },
            if entry_kept { "The program stays in it for your own settings." } else { "Recall's program entry was removed." }
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_java_runtimes_are_targets() {
        assert!(is_java(r"C:\x\bin\javaw.exe"));
        assert!(is_java(r"C:\x\bin\JAVA.EXE"));
        for t in [r"C:\Windows\explorer.exe", r"C:\x\javaw.exe.bak", r"C:\x\notjavaw.exe"] {
            assert!(!is_java(t), "{t}");
        }
    }

    #[test]
    fn undo_refuses_a_receipt_for_another_program_or_profile() {
        let t = r"C:\x\bin\javaw.exe";
        let ok = || Undo::DriverOwnProfile { vendor: Vendor::Nvidia, exe: t.into(), setting_id: pm::ID, value: pm::LAYERED_ON_DXGI, profile: FIX.own_profile(t) };
        let mut bad = Vec::new();
        for f in [
            |u: &mut Undo| { let Undo::DriverOwnProfile { exe, .. } = u; *exe = r"C:\y\bin\javaw.exe".into() },
            |u: &mut Undo| { let Undo::DriverOwnProfile { setting_id, .. } = u; *setting_id = 0x105E_2A1D },
            |u: &mut Undo| { let Undo::DriverOwnProfile { value, .. } = u; *value = pm::NATIVE },
            |u: &mut Undo| { let Undo::DriverOwnProfile { profile, .. } = u; *profile = "Java".into() },
        ] {
            let mut u = ok();
            f(&mut u);
            bad.push(u);
        }
        for u in bad {
            assert!(undo_minecraft(t, &u).unwrap_err().contains("does not match"), "{u:?}");
        }
    }

    #[test]
    fn the_profile_name_is_per_program_and_the_probe_is_a_sibling() {
        let a = FIX.own_profile(r"C:\a\javaw.exe");
        assert!(a.starts_with("Recall: Minecraft "), "{a}");
        assert_ne!(a, FIX.own_profile(r"C:\b\javaw.exe"));
        assert_eq!(a, FIX.own_profile(r"C:\A\JAVAW.EXE"));
        let probe = probe_path(r"C:\a\bin\javaw.exe");
        assert_eq!(probe, r"C:\a\bin\recall-path-check\javaw.exe");
        assert!(!Path::new(&probe).exists());
    }
}
