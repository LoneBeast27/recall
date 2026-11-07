//! The Recycle Bin precheck over injected facts and an injected registry (read only).

use super::*;

const GIB: u64 = 1 << 30;
const C_GUID: &str = "{6b9a6f8e-84ea-42bb-8615-8aea7bdcfa86}";

fn volume(id: &str, label: &str) -> Option<Volume> {
    Some(Volume { id: id.into(), label: label.into() })
}

fn local(settings: Settings) -> BinFacts {
    BinFacts {
        subst: false,
        resolved_unc: false,
        volume: volume(r"\\?\Volume{6b9a6f8e-84ea-42bb-8615-8aea7bdcfa86}\", "C:"),
        drive_type: DRIVE_FIXED,
        total_bytes: Some(1024 * GIB),
        bin_used: Some(0),
        settings,
    }
}

fn keyed(max_mb: u32) -> Settings {
    Settings { nuke: Some(0), max_mb: Some(max_mb), ..Settings::default() }
}

#[test]
fn only_a_local_enabled_bin_big_enough_takes_a_clip() {
    // 1,000 MB into a 3,000 MB bin: room for it and the 1 GiB margin.
    assert_eq!(decide(&local(keyed(3000)), 1000 << 20), Ok(()));
    assert!(decide(&local(keyed(1000)), (1000 << 20) + 1).unwrap_err().contains("bigger than the Recycle Bin"));
    let share = BinFacts { volume: volume(r"\\nas\clips\", r"\\nas\clips"), ..local(keyed(1000)) };
    assert!(decide(&share, 1).unwrap_err().contains("no Recycle Bin"), "a share is never recycled into");
    assert!(decide(&BinFacts { volume: None, ..local(keyed(1000)) }, 1).is_err(), "unknown drive");
    assert!(decide(&BinFacts { drive_type: 2, ..local(keyed(1000)) }, 1).unwrap_err().contains("removable"));
    assert!(decide(&BinFacts { drive_type: 4, ..local(keyed(1000)) }, 1).unwrap_err().contains("network"), "DRIVE_REMOTE");
    assert!(decide(&BinFacts { subst: true, ..local(keyed(1000)) }, 1).unwrap_err().contains("subst"));
    assert!(decide(&BinFacts { resolved_unc: true, ..local(keyed(1000)) }, 1).unwrap_err().contains("network share"));
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
    assert_eq!(decide(&local(policy.clone()), 9 * GIB), Ok(()), "9 GiB + the 1 GiB margin fit 10.24 GiB");
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
fn this_machine_reports_its_temp_volume_bin() {
    // Read only: the temp folder's volume is local and fixed with a bin; the answer is printed for the record.
    let f = facts(&std::env::temp_dir());
    println!("temp volume bin facts: {f:?}; default size {} MB", f.total_bytes.map_or(0, default_max_mb));
    assert_eq!(f.drive_type, DRIVE_FIXED, "{f:?}");
    assert!(f.volume.as_ref().is_some_and(|v| v.id.starts_with(r"\\?\Volume{")));
    assert!(f.total_bytes.is_some_and(|t| t > 0));
    // Review 11-07: the bin's used bytes must read on a real drive (a wrong SHQUERYRBINFO
    // size gives E_INVALIDARG, and refuse-when-unreadable would then refuse every delete).
    let root = format!("{}\\", f.volume.as_ref().map(|v| v.label.as_str()).unwrap_or_default());
    assert!(bin_used(&root).is_some(), "SHQueryRecycleBinW failed for {root}");
}

fn subst_of_l(letter: char) -> Option<String> {
    (letter == 'Q').then(|| r"\??\L:\General\sandbox".to_string())
}

fn real_volume(_: char) -> Option<String> {
    Some(r"\Device\HarddiskVolume3".into())
}

#[test]
fn a_subst_drive_is_refused_before_it_is_resolved() {
    assert!(is_subst(Path::new(r"Q:\Clips"), subst_of_l));
    assert!(!is_subst(Path::new(r"C:\Clips"), subst_of_l));
    assert!(!is_subst(Path::new(r"Q:\Clips"), real_volume));
    assert!(!is_subst(Path::new(r"\\nas\clips"), subst_of_l), "no drive letter");
    let f = facts_with(Path::new(r"Q:\Clips"), subst_of_l);
    assert!(f.subst && f.volume.is_none(), "{f:?}");
    assert!(decide(&f, 1).unwrap_err().contains("subst"));
}

#[test]
fn this_machine_names_its_temp_drive_a_real_volume() {
    let temp = std::env::temp_dir();
    let letter = letter_of(&temp).expect("temp has a drive letter");
    let target = dos_target(letter);
    println!("temp drive {letter}: -> {target:?}");
    assert!(target.is_some_and(|t| t.starts_with(r"\Device\")));
}

#[test]
fn a_clip_must_fit_the_bins_free_room_not_just_its_size() {
    // A 100 GiB bin already holding 90 GiB: a 6 GiB clip fits the size but not the room left
    // less the 5 GiB margin.
    let full = BinFacts { bin_used: Some(90 * GIB), ..local(keyed(100 * 1024)) };
    assert!(decide(&full, 6 * GIB).unwrap_err().contains("room"), "Windows would purge older bin items for it");
    assert_eq!(decide(&full, 5 * GIB), Ok(()), "it fits the free room");
    let unknown = BinFacts { bin_used: None, ..local(keyed(1000)) };
    assert!(decide(&unknown, 1).unwrap_err().contains("how full"), "an unreadable bin refuses");
}

#[test]
#[ignore]
fn bin_usage_on_this_machine() {
    // Read only. Run: cargo test -p recall-hdr --offline bin_usage_on_this_machine -- --ignored --nocapture
    for letter in 'A'..='Z' {
        let root = format!("{letter}:\\");
        if Path::new(&root).exists() {
            let f = facts(Path::new(&root));
            println!("BIN {letter}: used {:?} bytes; max {:?} MB; drive type {}", bin_used(&root), f.settings.max_mb, f.drive_type);
        }
    }
}

#[test]
fn a_clip_needs_a_margin_of_room_left_in_the_bin() {
    // A 100 GiB bin holding 50 GiB: room 50 GiB. A clip 1 KiB under the room would leave Windows
    // no slack (cluster rounding, a late `used`): refused. The margin is max(1 GiB, 5% of the bin).
    let half = BinFacts { bin_used: Some(50 * GIB), ..local(keyed(100 * 1024)) };
    assert!(decide(&half, 50 * GIB - 1024).unwrap_err().contains("room"), "room minus 1 KiB is too tight");
    assert!(decide(&half, 45 * GIB + 1).is_err(), "5% of a 100 GiB bin is 5 GiB");
    assert_eq!(decide(&half, 45 * GIB), Ok(()), "room minus the 5 GiB margin fits");
    // A small bin: the 1 GiB floor.
    let small = BinFacts { bin_used: Some(0), ..local(keyed(10 * 1024)) };
    assert!(decide(&small, 9 * GIB + 1).is_err());
    assert_eq!(decide(&small, 9 * GIB), Ok(()));
}
