//! The NVIDIA side of the fixer: the Minecraft overlay fix's plan (read-only)
//! and its helper actions (the writes). Kept apart so AMD and Intel can
//! sit beside it.

use std::path::Path;

use recall_fixes::plan::{minecraft_overlay_nvidia, new_profile_name, DriverAppSetting, Plan, Profile, Vendor};
use recall_fixes::{DriverPrior, FixId, Undo};
use recall_nvidia::{present_method as pm, DriverWriter, Prior};

/// A Java runtime: the only programs this fix may touch.
pub fn is_java(target: &str) -> bool {
    Path::new(target).file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case("javaw.exe") || n.eq_ignore_ascii_case("java.exe"))
}

fn check_target(target: &str) -> Result<(), String> {
    if !is_java(target) {
        return Err(format!("{target} is not a Java runtime"));
    }
    if !Path::new(target).is_file() {
        return Err(format!("{target} does not exist"));
    }
    Ok(())
}

/// Read-only: the plan for `exe`, or `None` when the setting is already right.
pub fn minecraft_plan(exe: &str) -> Result<Option<Plan>, String> {
    check_target(exe)?;
    let now = DriverWriter::load()?.read(exe, pm::ID)?;
    if now.value == Some(pm::LAYERED_ON_DXGI) {
        return Ok(None);
    }
    let profile = match now.profile {
        Some(name) => Profile::Existing(name),
        None => Profile::None { new_profile: new_profile_name(FixId::MinecraftOverlayNvidia, exe) },
    };
    let setting = DriverAppSetting {
        vendor: Vendor::Nvidia,
        exe: exe.to_owned(),
        setting_id: pm::ID,
        setting_name: pm::NAME.to_owned(),
        profile,
        from: now.value.map(|v| (v, pm::value_name(v))),
        to: (pm::LAYERED_ON_DXGI, pm::value_name(pm::LAYERED_ON_DXGI)),
    };
    Ok(Some(minecraft_overlay_nvidia(exe, setting)))
}

/// Elevated helper: set the present method for `target`.
pub fn apply_minecraft(target: &str) -> Result<Undo, String> {
    check_target(target)?;
    let prior = DriverWriter::load()?.apply(target, pm::ID, pm::LAYERED_ON_DXGI, &new_profile_name(FixId::MinecraftOverlayNvidia, target))?;
    Ok(Undo::DriverAppSetting { vendor: Vendor::Nvidia, exe: target.to_owned(), setting_id: pm::ID, prior: to_receipt(prior) })
}

/// Elevated helper: reverse one receipt entry, after checking it could only
/// have come from [`apply_minecraft`] for this same program.
pub fn undo_minecraft(target: &str, undo: &Undo) -> Result<(), String> {
    let Undo::DriverAppSetting { vendor: Vendor::Nvidia, exe, setting_id, prior } = undo;
    if !exe.eq_ignore_ascii_case(target) || *setting_id != pm::ID || !is_java(target) {
        return Err("the undo receipt does not match this fix".into());
    }
    let nv = DriverWriter::load()?;
    match prior {
        DriverPrior::CreatedProfile { profile } if *profile != new_profile_name(FixId::MinecraftOverlayNvidia, target) => {
            return Err(format!("refusing to delete the profile \"{profile}\": Recall did not make it for this program"));
        }
        DriverPrior::CreatedProfile { .. } => {}
        DriverPrior::SettingAdded { profile } | DriverPrior::PredefinedValue { profile, .. } | DriverPrior::UserValue { profile, .. } => {
            // Only the profile that holds this program today may be touched.
            if nv.read(target, pm::ID)?.profile.as_deref() != Some(profile.as_str()) {
                return Err(format!("the profile \"{profile}\" no longer holds this program; nothing was changed"));
            }
            if let DriverPrior::PredefinedValue { old, .. } | DriverPrior::UserValue { old, .. } = prior {
                if ![pm::NATIVE, pm::LAYERED_ON_DXGI, pm::AUTO].contains(old) {
                    return Err(format!("the undo receipt holds an unknown value {old}"));
                }
            }
        }
    }
    nv.revert(pm::ID, &from_receipt(prior))
}

fn to_receipt(p: Prior) -> DriverPrior {
    match p {
        Prior::CreatedProfile { profile } => DriverPrior::CreatedProfile { profile },
        Prior::SettingAdded { profile } => DriverPrior::SettingAdded { profile },
        Prior::PredefinedValue { profile, old } => DriverPrior::PredefinedValue { profile, old },
        Prior::UserValue { profile, old } => DriverPrior::UserValue { profile, old },
    }
}

fn from_receipt(p: &DriverPrior) -> Prior {
    match p.clone() {
        DriverPrior::CreatedProfile { profile } => Prior::CreatedProfile { profile },
        DriverPrior::SettingAdded { profile } => Prior::SettingAdded { profile },
        DriverPrior::PredefinedValue { profile, old } => Prior::PredefinedValue { profile, old },
        DriverPrior::UserValue { profile, old } => Prior::UserValue { profile, old },
    }
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
        let other = Undo::DriverAppSetting { vendor: Vendor::Nvidia, exe: r"C:\y\bin\javaw.exe".into(), setting_id: pm::ID, prior: DriverPrior::CreatedProfile { profile: "x".into() } };
        assert!(undo_minecraft(t, &other).unwrap_err().contains("does not match"));
        let wrong_id = Undo::DriverAppSetting { vendor: Vendor::Nvidia, exe: t.into(), setting_id: 0x105E_2A1D, prior: DriverPrior::CreatedProfile { profile: "x".into() } };
        assert!(undo_minecraft(t, &wrong_id).unwrap_err().contains("does not match"));
    }

    #[test]
    fn the_profile_name_is_per_program() {
        let a = new_profile_name(FixId::MinecraftOverlayNvidia, r"C:\a\javaw.exe");
        assert!(a.starts_with("Recall: Minecraft "), "{a}");
        assert_ne!(a, new_profile_name(FixId::MinecraftOverlayNvidia, r"C:\b\javaw.exe"));
        assert_eq!(a, new_profile_name(FixId::MinecraftOverlayNvidia, r"C:\A\JAVAW.EXE"));
    }
}
