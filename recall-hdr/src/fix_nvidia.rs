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

/// The driver entry for `exe`: its last three path parts, lower case, `/`-joined
/// (`zulu25…-win_x64/bin/javaw.exe`), the form NVIDIA's own profiles use; it keeps the
/// entry to this one Java install.
///
/// A build folder without a digit (`jre`, `runtime`, `jdk`, `java`, `java-runtime-delta`) names
/// no build: any app shipping a folder of that name would match. Then the folder above it
/// joins the entry (four parts), as NVIDIA's own entries do (`dungeons/binaries/win64/…`).
pub fn drs_entry(exe: &str) -> Result<String, String> {
    let parts: Vec<&str> = exe.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    let n = parts.len();
    let take = if n >= 3 && parts[n - 3].chars().any(|c| c.is_ascii_digit()) { 3 } else { 4 };
    // One more part than taken: the drive or a folder above, so the entry never is the whole path.
    if n < take + 1 {
        return Err("the program's path is too short to name one Java build".into());
    }
    Ok(parts[n - take..].join("/").to_lowercase())
}

/// Made-up programs that must not match Recall's entry: the same file in another install
/// folder beside this one, and the bare file name.
fn probes(exe: &str) -> Vec<String> {
    let p = Path::new(exe);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("javaw.exe").to_owned();
    let installs = p.parent().and_then(Path::parent).and_then(Path::parent).map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = vec![format!("{installs}\\recall-path-check\\bin\\{name}"), name];
    // A four-part entry: the same generic folder under another parent must not match either.
    let entry = drs_entry(exe).unwrap_or_default();
    let above = p.parent().and_then(Path::parent).and_then(Path::parent).and_then(Path::parent);
    if let (4, Some(above)) = (entry.split('/').count(), above) {
        let build = entry.split('/').nth(1).unwrap_or("runtime").to_owned();
        let probe = format!("{}\\recall-path-check\\{build}\\bin\\{}", above.to_string_lossy(), out[1]);
        out.push(probe);
    }
    out
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
            entry: drs_entry(exe)?,
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
    let probes = probes(target);
    let probes: Vec<&str> = probes.iter().map(String::as_str).collect();
    DriverWriter::load()?.create_own(target, &drs_entry(target)?, &probes, pm::ID, pm::LAYERED_ON_DXGI, &profile)?;
    Ok(Undo::DriverOwnProfile { vendor: Vendor::Nvidia, exe: target.to_owned(), setting_id: pm::ID, value: pm::LAYERED_ON_DXGI, profile, entry: drs_entry(target)? })
}

/// Helper: undo one receipt entry, after checking it could only have come
/// from [`apply_minecraft`] for this same program. Returns a note for the
/// user when only part of the profile was Recall's.
pub fn undo_minecraft(target: &str, undo: &Undo) -> Result<Option<String>, String> {
    let Undo::DriverOwnProfile { vendor: Vendor::Nvidia, exe, setting_id, value, profile, entry } = undo else {
        return Err("the undo receipt does not match this fix".into());
    };
    // An old receipt (no entry) wrote the full path itself; a new one, the end of the path.
    let entry = if entry.is_empty() { exe.clone() } else { entry.clone() };
    if !entry.eq_ignore_ascii_case(exe) && Ok(entry.clone()) != drs_entry(target) {
        return Err("the undo receipt does not match this fix".into());
    }
    if !exe.eq_ignore_ascii_case(target) || *setting_id != pm::ID || *value != pm::LAYERED_ON_DXGI || !is_java(target) || *profile != FIX.own_profile(target) {
        return Err("the undo receipt does not match this fix".into());
    }
    Ok(match DriverWriter::load()?.revert_own(&entry, pm::ID, pm::LAYERED_ON_DXGI, profile)? {
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
        let ok = || Undo::DriverOwnProfile { vendor: Vendor::Nvidia, exe: t.into(), setting_id: pm::ID, value: pm::LAYERED_ON_DXGI, profile: FIX.own_profile(t), entry: String::new() };
        let mut bad = Vec::new();
        for f in [
            |u: &mut Undo| { if let Undo::DriverOwnProfile { exe, .. } = u { *exe = r"C:\y\bin\javaw.exe".into() } },
            |u: &mut Undo| { if let Undo::DriverOwnProfile { setting_id, .. } = u { *setting_id = 0x105E_2A1D } },
            |u: &mut Undo| { if let Undo::DriverOwnProfile { value, .. } = u { *value = pm::NATIVE } },
            |u: &mut Undo| { if let Undo::DriverOwnProfile { profile, .. } = u { *profile = "Java".into() } },
            |u: &mut Undo| { if let Undo::DriverOwnProfile { entry, .. } = u { *entry = "other/bin/javaw.exe".into() } },
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
        let exe = r"C:\Users\u\AppData\Roaming\ModrinthApp\meta\java_versions\zulu25.38.109-ca-jre25.0.5.0.3-win_x64\bin\javaw.exe";
        assert_eq!(drs_entry(exe).unwrap(), "zulu25.38.109-ca-jre25.0.5.0.3-win_x64/bin/javaw.exe");
        assert!(drs_entry(r"C:\javaw.exe").is_err());
        let ps = probes(exe);
        assert_eq!(ps[0], r"C:\Users\u\AppData\Roaming\ModrinthApp\meta\java_versions\recall-path-check\bin\javaw.exe");
        assert_eq!(ps[1], "javaw.exe");
        assert_ne!(drs_entry(&ps[0]).unwrap(), drs_entry(exe).unwrap());
        // Another Java build never shares the entry; the same build elsewhere does (by design).
        let other_build = r"C:\Users\u\AppData\Roaming\ModrinthApp\meta\java_versions\zulu21.44.17-ca-jre21.0.8-win_x64\bin\javaw.exe";
        assert_ne!(drs_entry(other_build).unwrap(), drs_entry(exe).unwrap());
        let same_build_elsewhere = r"D:\Launcher\runtime\zulu25.38.109-ca-jre25.0.5.0.3-win_x64\bin\javaw.exe";
        assert_eq!(drs_entry(same_build_elsewhere).unwrap(), drs_entry(exe).unwrap());
        // Entry and probes share one form: lower case, forward slashes, last three parts.
        assert_eq!(drs_entry(&exe.to_uppercase()).unwrap(), drs_entry(exe).unwrap());
        assert_eq!(drs_entry(&exe.replace('\\', "/")).unwrap(), drs_entry(exe).unwrap());
        // Generic folder names name no build: the folder above joins the entry.
        for (path, want) in [
            (r"C:\Users\u\AppData\Roaming\.minecraft\runtime\java-runtime-delta\windows-x64\java-runtime-delta\bin\javaw.exe", "windows-x64/java-runtime-delta/bin/javaw.exe"),
            (r"C:\Games\SomeLauncher\jre\bin\javaw.exe", "somelauncher/jre/bin/javaw.exe"),
            (r"D:\Tools\App\runtime\bin\java.exe", "app/runtime/bin/java.exe"),
        ] {
            assert_eq!(drs_entry(path).unwrap(), want, "{path}");
        }
        assert!(drs_entry(r"C:\jre\bin\javaw.exe").is_err(), "a generic folder at the root names nothing");
        let generic = r"C:\Games\SomeLauncher\jre\bin\javaw.exe";
        let ps = probes(generic);
        assert_eq!(ps.len(), 3);
        assert_eq!(ps[2], r"C:\Games\recall-path-check\jre\bin\javaw.exe");
        assert_ne!(drs_entry(&ps[2]).unwrap(), drs_entry(generic).unwrap());
        assert!(!Path::new(&probe).exists());
    }
}
