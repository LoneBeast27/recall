use crate::helper::{check_target, new_nonce, result_path, Request};
use crate::plan::{minecraft_overlay_nvidia, Change, DriverAppSetting, FixId, Profile, Vendor};
use crate::receipt::{DriverPrior, Receipt, Store, Undo};

const JAVAW: &str = r"C:\Users\u\AppData\Roaming\ModrinthApp\meta\java_versions\zulu25\bin\javaw.exe";

fn setting(profile: Profile, from: Option<(u32, String)>) -> DriverAppSetting {
    DriverAppSetting {
        vendor: Vendor::Nvidia,
        exe: JAVAW.into(),
        setting_id: 0x20D6_90F8,
        setting_name: "Vulkan/OpenGL present method".into(),
        profile,
        from,
        to: (1, "Prefer layered on DXGI Swapchain".into()),
    }
}

fn new_profile() -> Profile {
    Profile::None { new_profile: "Recall: Minecraft".into() }
}

#[test]
fn the_i_view_names_every_exact_change() {
    let plan = minecraft_overlay_nvidia(JAVAW, setting(new_profile(), None));
    assert!(!plan.needs_admin(), "the NVIDIA database is writable without elevation");
    let lines = plan.steps[0].change.exact();
    let all = lines.join("\n");
    for want in [JAVAW, "\"Recall: Minecraft\"", "0x20D690F8", "not set, driver default → Prefer layered on DXGI Swapchain (1)", "CreateProfile, CreateApplication, SetSetting, SaveSettings", "No administrator prompt", "Undo: delete the profile"] {
        assert!(all.contains(want), "missing {want:?} in:\n{all}");
    }
}

#[test]
fn an_existing_profile_is_named_and_only_its_setting_changes() {
    let c = Change::DriverAppSetting(setting(Profile::Existing("Java".into()), Some((2, "Auto".into()))));
    let all = c.exact().join("\n");
    assert!(all.contains("\"Java\" (already holds this program"), "{all}");
    assert!(all.contains("Auto (2) → Prefer layered"), "{all}");
    assert!(all.contains("Driver calls (NvAPI DRS): SetSetting, SaveSettings"), "no profile is created: {all}");
    assert!(all.contains("Undo: put the setting back to Auto (2)"), "{all}");
}

#[test]
fn fix_ids_round_trip_and_unknown_ones_are_refused() {
    for f in FixId::ALL {
        assert_eq!(FixId::parse(f.as_str()), Some(f));
    }
    assert_eq!(FixId::parse("format-c"), None);
}

#[test]
fn requests_round_trip_through_the_command_line() {
    let nonce = new_nonce();
    let undo = Undo::DriverAppSetting { vendor: Vendor::Nvidia, exe: JAVAW.into(), setting_id: 0x20D6_90F8, prior: DriverPrior::UserValue { profile: "Java \"x\" ".into(), old: 2 } };
    for req in [
        Request::Apply { fix: FixId::MinecraftOverlayNvidia, nonce: nonce.clone(), target: JAVAW.into() },
        Request::Undo { fix: FixId::MinecraftOverlayNvidia, nonce: nonce.clone(), target: JAVAW.into(), undo },
    ] {
        let line = req.arguments().unwrap();
        let args = split_like_windows(&line);
        assert_eq!(Request::parse(&args).unwrap(), req, "{line}");
    }
}

#[test]
fn the_helper_refuses_anything_else() {
    let ok_nonce = "0123456789abcdef0123456789abcdef".to_owned();
    let bad: Vec<Vec<String>> = vec![
        vec![],
        vec!["--record".into()],
        vec!["--fix-apply".into(), "nope".into(), ok_nonce.clone(), JAVAW.into()],
        vec!["--fix-apply".into(), "minecraft-overlay-nvidia".into(), "..\\x".into(), JAVAW.into()],
        vec!["--fix-apply".into(), "minecraft-overlay-nvidia".into(), ok_nonce.clone(), "javaw.exe".into()],
        vec!["--fix-apply".into(), "minecraft-overlay-nvidia".into(), ok_nonce.clone(), JAVAW.into(), "extra".into()],
        vec!["--fix-undo".into(), "minecraft-overlay-nvidia".into(), ok_nonce.clone(), JAVAW.into(), "zz".into()],
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
fn nonces_are_32_hex_and_differ() {
    let (a, b) = (new_nonce(), new_nonce());
    assert_eq!(a.len(), 32);
    assert!(crate::helper::check_nonce(&a).is_ok());
    assert_ne!(a, b);
}

#[test]
fn receipts_save_list_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("applied"));
    assert!(store.list().is_empty(), "no folder yet: nothing applied");
    let plan = minecraft_overlay_nvidia(JAVAW, setting(new_profile(), None));
    let receipt = Receipt {
        fix: plan.fix,
        target: JAVAW.into(),
        applied_unix: 1_794_483_000,
        plan,
        undo: vec![Undo::DriverAppSetting { vendor: Vendor::Nvidia, exe: JAVAW.into(), setting_id: 0x20D6_90F8, prior: DriverPrior::CreatedProfile { profile: "Recall: Minecraft".into() } }],
    };
    store.save(&receipt).unwrap();
    std::fs::write(dir.path().join("applied").join("damaged.json"), b"{not json").unwrap();
    assert_eq!(store.list(), vec![receipt.clone()], "a damaged file is skipped");
    assert!(dir.path().join("applied").join("damaged.json").exists(), "and kept");
    assert_eq!(store.find(FixId::MinecraftOverlayNvidia, &JAVAW.to_uppercase()), Some(receipt), "paths match case-insensitively");
    store.remove(FixId::MinecraftOverlayNvidia, JAVAW).unwrap();
    assert!(store.find(FixId::MinecraftOverlayNvidia, JAVAW).is_none());
    store.remove(FixId::MinecraftOverlayNvidia, JAVAW).unwrap();
}

#[test]
fn a_test_build_never_prompts() {
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
