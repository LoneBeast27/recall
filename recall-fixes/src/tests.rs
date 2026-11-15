use std::path::PathBuf;

use crate::helper::{check_target, new_nonce, result_path, Outcome, Request};
use crate::offer::{Dismissed, Offer};
use crate::plan::{minecraft_overlay_nvidia, DriverAppSetting, FixId, Plan, Vendor};
use crate::receipt::{target_hash, Receipt, Store, Undo};

const JAVAW: &str = r"C:\Users\u\AppData\Roaming\ModrinthApp\meta\java_versions\zulu25\bin\javaw.exe";

/// A fresh folder under the system temp folder, removed on drop (no extra crate).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("recall-fixes-test-{tag}-{}-{}", std::process::id(), new_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn plan() -> Plan {
    minecraft_overlay_nvidia(DriverAppSetting {
        vendor: Vendor::Nvidia,
        exe: JAVAW.into(),
        entry: "zulu25/bin/javaw.exe".into(),
        setting_id: 0x20D6_90F8,
        setting_name: "Vulkan/OpenGL present method".into(),
        new_profile: FixId::MinecraftOverlayNvidia.own_profile(JAVAW),
        to: (1, "Prefer layered on DXGI Swapchain".into()),
    })
}

fn undo() -> Undo {
    Undo::DriverOwnProfile { vendor: Vendor::Nvidia, exe: JAVAW.into(), setting_id: 0x20D6_90F8, value: 1, profile: FixId::MinecraftOverlayNvidia.own_profile(JAVAW), entry: "zulu25/bin/javaw.exe".into() }
}

#[test]
fn the_i_view_names_every_exact_change_and_check() {
    let plan = plan();
    assert!(!plan.needs_admin(), "the NVIDIA database is writable without elevation");
    let all = plan.steps[0].change.exact().join("\n");
    for want in [
        JAVAW,
        "every program that runs on the Java build \"zulu25\" gets the setting, wherever that build is installed",
        "Other launchers may bundle the same build",
        "No existing profile is changed",
        "0x20D690F8",
        "Auto) → Prefer layered on DXGI Swapchain (1)",
        "CreateProfile, CreateApplication, SetSetting, SaveSettings",
        "nothing is saved",
        "Driver entry: \"zulu25/bin/javaw.exe\"",
        "No administrator prompt",
        "Undo: delete",
        "while it still has Recall's value",
    ] {
        assert!(all.contains(want), "missing {want:?} in:\n{all}");
    }
}

#[test]
fn old_receipts_without_an_entry_still_read() {
    let old = r#"{"DriverOwnProfile":{"vendor":"Nvidia","exe":"C:\\j\\bin\\javaw.exe","setting_id":551981304,"value":1,"profile":"Recall: Minecraft 1"}}"#;
    let Undo::DriverOwnProfile { entry, exe, .. } = serde_json::from_str::<Undo>(old).unwrap() else { panic!("an NVIDIA receipt") };
    assert!(entry.is_empty(), "no entry: undo uses the full path it wrote");
    assert_eq!(exe, r"C:\j\bin\javaw.exe");
}

#[test]
fn the_build_is_named_plainly() {
    let mut s = DriverAppSetting {
        vendor: Vendor::Nvidia,
        exe: JAVAW.into(),
        entry: "zulu25.38.109-ca-jre25.0.5.0.3-win_x64/bin/javaw.exe".into(),
        setting_id: 0x20D6_90F8,
        setting_name: "Vulkan/OpenGL present method".into(),
        new_profile: "Recall: Minecraft 1".into(),
        to: (1, "Prefer layered on DXGI Swapchain".into()),
    };
    assert_eq!(s.build(), "zulu25.38.109-ca-jre25.0.5.0.3-win_x64");
    s.entry = "somelauncher/jre/bin/javaw.exe".into();
    assert_eq!(s.build(), "somelauncher/jre");
    s.entry = String::new();
    assert_eq!(s.build(), JAVAW, "an old full-path entry names the whole path");
}

#[test]
fn the_fps_fix_names_the_group_the_account_and_both_admin_steps() {
    let plan = crate::plan::fps_permission("S-1-5-21-1-2-3-1001");
    assert!(plan.needs_admin(), "local groups need an administrator");
    let all = plan.steps[0].change.exact().join("\n");
    for want in ["\"Performance Log Users\" (S-1-5-32-559)", "S-1-5-21-1-2-3-1001", "/add-perflog-user S-1-5-21-1-2-3-1001", "/remove-perflog-user S-1-5-21-1-2-3-1001", "sign out"] {
        assert!(all.contains(want), "missing {want:?} in:\n{all}");
    }
    assert_eq!(FixId::parse("fps-permission"), Some(FixId::FpsPermission));
}

#[test]
fn titles_come_from_the_fix_id() {
    let f = FixId::MinecraftOverlayNvidia;
    assert_eq!(f.title(), "Show the overlay in Minecraft");
    assert!(f.problem().contains("borderless and fullscreen"));
    assert_eq!(f.after(), Some("Restart Minecraft"));
}

#[test]
fn fix_ids_round_trip_and_unknown_ones_are_refused() {
    for f in FixId::ALL {
        assert_eq!(FixId::parse(f.as_str()), Some(f));
    }
    assert_eq!(FixId::parse("format-c"), None);
}

#[test]
fn own_profiles_are_per_program() {
    let f = FixId::MinecraftOverlayNvidia;
    assert!(f.own_profile(JAVAW).starts_with("Recall: Minecraft "));
    assert_eq!(f.own_profile(JAVAW), f.own_profile(&JAVAW.to_uppercase()));
    assert_ne!(f.own_profile(JAVAW), f.own_profile(r"C:\other\javaw.exe"));
}

#[test]
fn requests_round_trip_through_the_command_line() {
    let nonce = new_nonce();
    for req in [
        Request::Apply { fix: FixId::MinecraftOverlayNvidia, nonce: nonce.clone(), target: JAVAW.into(), plan: plan() },
        Request::Undo { fix: FixId::MinecraftOverlayNvidia, nonce: nonce.clone(), target: JAVAW.into(), undo: undo() },
    ] {
        let line = req.arguments().unwrap();
        assert_eq!(Request::parse(&split_like_windows(&line)).unwrap(), req, "{line}");
    }
}

#[test]
fn the_helper_refuses_anything_else() {
    let n = "0123456789abcdef0123456789abcdef".to_owned();
    let fix = "minecraft-overlay-nvidia".to_owned();
    let a = "--fix-apply".to_owned();
    let bad: Vec<Vec<String>> = vec![
        vec![],
        vec!["--record".into()],
        vec![a.clone(), "nope".into(), n.clone(), JAVAW.into(), "7b7d".into()],
        vec![a.clone(), fix.clone(), "..\\x".into(), JAVAW.into(), "7b7d".into()],
        vec![a.clone(), fix.clone(), n.clone(), "javaw.exe".into(), "7b7d".into()],
        vec![a.clone(), fix.clone(), n.clone(), JAVAW.into()],
        vec![a.clone(), fix.clone(), n.clone(), JAVAW.into(), "7b7d".into()],
        vec!["--fix-undo".into(), fix.clone(), n.clone(), JAVAW.into(), "zz".into()],
    ];
    for args in bad {
        assert!(Request::parse(&args).is_err(), "{args:?} must be refused");
    }
    for t in [r"\\server\share\javaw.exe", r"C:\a\..\Windows\javaw.exe", "C:\\a\"b\\javaw.exe", r"relative\javaw.exe"] {
        assert!(check_target(t).is_err(), "{t}");
    }
    assert!(result_path(std::path::Path::new("C:\\r"), "../../evil").is_err());
}

#[test]
fn results_carry_their_request_and_plan() {
    let o = Outcome { nonce: new_nonce(), plan_hash: plan().hash(), error: None, note: None };
    let back: Outcome = serde_json::from_slice(&serde_json::to_vec(&o).unwrap()).unwrap();
    assert_eq!(back, o);
    assert_eq!(plan().hash(), plan().hash(), "stable");
    let mut other = plan();
    other.steps.clear();
    assert_ne!(other.hash(), plan().hash());
}

#[test]
fn nonces_are_32_hex_and_differ() {
    let (a, b) = (new_nonce(), new_nonce());
    assert_eq!(a.len(), 32);
    assert!(crate::helper::check_nonce(&a).is_ok());
    assert_ne!(a, b);
}

#[test]
fn receipts_save_list_and_remove() {
    let dir = TempDir::new("receipts");
    let store = Store::new(dir.0.join("applied"));
    assert!(store.list().is_empty(), "no folder yet: nothing applied");
    let receipt = Receipt { fix: FixId::MinecraftOverlayNvidia, target: JAVAW.into(), applied_unix: 1_794_483_000, plan: plan(), undo: vec![undo()] };
    store.save(&receipt).unwrap();
    std::fs::write(dir.0.join("applied").join("damaged.json"), b"{not json").unwrap();
    assert_eq!(store.list(), vec![receipt.clone()], "a damaged file is skipped");
    assert!(dir.0.join("applied").join("damaged.json").exists(), "and kept");
    assert_eq!(store.find(FixId::MinecraftOverlayNvidia, &JAVAW.to_uppercase()), Some(receipt), "paths match case-insensitively");
    store.remove(FixId::MinecraftOverlayNvidia, JAVAW).unwrap();
    assert!(store.find(FixId::MinecraftOverlayNvidia, JAVAW).is_none());
    store.remove(FixId::MinecraftOverlayNvidia, JAVAW).unwrap();
}

#[test]
fn offers_save_list_and_go_away() {
    let dir = TempDir::new("offers");
    let offers = dir.0.join("offers");
    assert!(Offer::list(&offers).is_empty());
    let offer = Offer::new(plan(), JAVAW);
    offer.save(&offers).unwrap();
    offer.save(&offers).unwrap();
    assert_eq!(Offer::list(&offers), vec![offer.clone()], "one file per fix and program");
    Offer::remove(&offers, FixId::MinecraftOverlayNvidia, JAVAW).unwrap();
    assert!(Offer::list(&offers).is_empty());
}

#[test]
fn not_now_survives_a_restart() {
    let dir = TempDir::new("dismissed");
    let fix = FixId::MinecraftOverlayNvidia;
    assert!(!Dismissed::new(&dir.0).contains(fix, JAVAW));
    Dismissed::new(&dir.0).add(fix, JAVAW).unwrap();
    // A new reader of the same folder (the next recorder or app run).
    let again = Dismissed::new(&dir.0);
    assert!(again.contains(fix, &JAVAW.to_uppercase()));
    assert!(!again.contains(fix, r"C:\other\javaw.exe"), "per program");
    let body = std::fs::read_to_string(dir.0.join("dismissed.json")).unwrap();
    assert!(!body.contains("Users") && body.contains(&target_hash(JAVAW)), "hashes, no paths: {body}");
}

#[test]
fn a_test_build_never_starts_anything() {
    assert_eq!(crate::elevate::run(std::path::Path::new("C:\\Windows\\notepad.exe"), "", true, false), crate::elevate::Started::Blocked);
}

/// Windows' rule for this simple case: spaces split, double quotes group.
fn split_like_windows(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in line.chars() {
        match ch {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Both Minecraft fixes stay off (never offered, still parsed so receipts undo); the Rocket
/// League Borderless fix is offered.
#[test]
fn only_the_fps_and_rocket_league_fixes_are_offered_and_all_are_known() {
    for fix in FixId::ALL {
        assert_eq!(fix.offered(), matches!(fix, FixId::FpsPermission | FixId::RocketLeagueBorderless), "{fix:?}");
        assert_eq!(FixId::parse(fix.as_str()), Some(fix));
    }
    assert!(FixId::FpsPermission.offered(), "the FPS fix is offered");
}
