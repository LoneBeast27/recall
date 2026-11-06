//! The Recycle Bin precheck over injected facts and an injected registry (read only).

use super::*;

const GIB: u64 = 1 << 30;
const C_GUID: &str = "{6b9a6f8e-84ea-42bb-8615-8aea7bdcfa86}";

fn volume(id: &str, label: &str) -> Option<Volume> {
    Some(Volume { id: id.into(), label: label.into() })
}

fn local(settings: Settings) -> BinFacts {
    BinFacts {
        volume: volume(r"\\?\Volume{6b9a6f8e-84ea-42bb-8615-8aea7bdcfa86}\", "C:"),
        drive_type: DRIVE_FIXED,
        total_bytes: Some(1024 * GIB),
        settings,
    }
}

fn keyed(max_mb: u32) -> Settings {
    Settings { nuke: Some(0), max_mb: Some(max_mb), ..Settings::default() }
}

#[test]
fn only_a_local_enabled_bin_big_enough_takes_a_clip() {
    assert_eq!(decide(&local(keyed(1000)), 1000 << 20), Ok(()));
    assert!(decide(&local(keyed(1000)), (1000 << 20) + 1).unwrap_err().contains("bigger than the Recycle Bin"));
    let share = BinFacts { volume: volume(r"\\nas\clips\", r"\\nas\clips"), ..local(keyed(1000)) };
    assert!(decide(&share, 1).unwrap_err().contains("no Recycle Bin"), "a share is never recycled into");
    assert!(decide(&BinFacts { volume: None, ..local(keyed(1000)) }, 1).is_err(), "unknown drive");
    assert!(decide(&BinFacts { drive_type: 2, ..local(keyed(1000)) }, 1).unwrap_err().contains("removable"));
    let off = Settings { nuke: Some(1), ..keyed(1000) };
    assert!(decide(&local(off), 1).unwrap_err().contains("turned off"));
}

#[test]
fn windows_default_size_rule() {
    assert_eq!(default_max_mb(32 * GIB), 32 * 1024 / 10, "up to 40 GB: 10%");
    assert_eq!(default_max_mb(40 * GIB), 4096);
    // This machine's V: (3,815,429 MiB) stores MaxCapacity 192,819; L: (1,907,727 MiB) 97,434.
    assert_eq!(default_max_mb(3_815_429 << 20), 192_819);
    assert_eq!(default_max_mb(1_907_727 << 20), 97_434);
}

#[test]
fn a_volume_without_a_bin_key_uses_windows_defaults() {
    // A new user's drive: no BitBucket\Volume key yet. 1 TiB -> 4 GiB + 5% of 984 GiB = 54,476 MB.
    let fresh = local(Settings::default());
    assert_eq!(decide(&fresh, 10 * GIB), Ok(()), "bin on, default size");
    assert!(decide(&fresh, 60 * GIB).unwrap_err().contains("54476 MB"));
    let no_size = BinFacts { total_bytes: None, ..fresh };
    assert!(decide(&no_size, 1).unwrap_err().contains("unknown"), "no size, no move");
}

#[test]
fn the_no_recycle_files_policy_refuses() {
    let policy = Settings { policy_off: true, ..keyed(100_000) };
    assert!(decide(&local(policy), 1).unwrap_err().contains("policy"));
}

#[test]
fn a_size_policy_wins_over_the_volume_setting() {
    // MaxCapacity says ~97 GB, the policy says 1% of 1 TiB (~10.2 GiB).
    let policy = Settings { policy_pct: Some(1), ..keyed(100_000) };
    assert_eq!(decide(&local(policy.clone()), 10 * GIB), Ok(()));
    assert!(decide(&local(policy), 20 * GIB).unwrap_err().contains("bigger than the Recycle Bin"));
}

/// HKLM: NoRecycleFiles 1, RecycleBinSize 5. HKCU: NoRecycleFiles 0, RecycleBinSize 10, and the volume key.
fn fake_registry(hive: Hive, key: &str, value: &str) -> Option<u32> {
    let policies = key == POLICY_KEY;
    match (hive, policies, value) {
        (Hive::Machine, true, "NoRecycleFiles") => Some(1),
        (Hive::Machine, true, "RecycleBinSize") => Some(5),
        (Hive::User, true, "NoRecycleFiles") => Some(0),
        (Hive::User, true, "RecycleBinSize") => Some(10),
        (Hive::User, false, "NukeOnDelete") if key.ends_with(C_GUID) => Some(0),
        (Hive::User, false, "MaxCapacity") if key.ends_with(C_GUID) => Some(49_703),
        _ => None,
    }
}

fn user_policy_only(hive: Hive, key: &str, value: &str) -> Option<u32> {
    if hive == Hive::Machine { None } else { fake_registry(hive, key, value) }
}

#[test]
fn settings_read_the_volume_key_and_both_policy_hives() {
    let s = read_settings(C_GUID, fake_registry);
    assert_eq!(s, Settings { nuke: Some(0), max_mb: Some(49_703), policy_off: true, policy_pct: Some(5) }, "HKLM wins");
    let s = read_settings(C_GUID, user_policy_only);
    assert_eq!((s.policy_off, s.policy_pct), (false, Some(10)), "HKCU applies without a machine policy");
    let s = read_settings("{00000000-0000-0000-0000-000000000000}", |_, _, _| None);
    assert_eq!(s, Settings::default(), "no key, no policy: Windows defaults");
}

#[test]
fn the_recycle_flags_never_delete_for_good_unasked() {
    assert!(recycles_only(RECYCLE_FLAGS));
    assert!(!recycles_only(FOF_NOCONFIRMATION | FOF_SILENT), "no FOF_ALLOWUNDO: a plain delete");
    assert!(!recycles_only(FOF_ALLOWUNDO | FOF_NOCONFIRMATION), "auto-yes to a permanent delete");
    assert!(recycles_only(FOF_ALLOWUNDO | FOF_SILENT));
}

#[test]
fn this_machine_reports_its_temp_volume_bin() {
    // Read only: the temp folder's volume is local and fixed with a bin; the answer is printed for the record.
    let f = facts(&std::env::temp_dir());
    println!("temp volume bin facts: {f:?}; default size {} MB", f.total_bytes.map_or(0, default_max_mb));
    assert_eq!(f.drive_type, DRIVE_FIXED, "{f:?}");
    assert!(f.volume.is_some_and(|v| v.id.starts_with(r"\\?\Volume{")));
    assert!(f.total_bytes.is_some_and(|t| t > 0));
}
