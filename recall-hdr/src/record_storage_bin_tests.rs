use super::*;

// ---- The bin check counts the clip AND its companions, and an unreadable size refuses ----

/// Every companion is 600 MiB.
fn big_companions(_: &Path) -> Option<u64> {
    Some(600 << 20)
}

/// A companion whose size cannot be read.
fn unreadable(path: &Path) -> Option<u64> {
    (!path.to_string_lossy().contains("recall-edit")).then_some(1)
}

/// A bin with 2 GiB of room.
fn two_gib_room(_: &Path, bytes: u64) -> Result<(), String> {
    if bytes > 2 * GIB { Err("no room left in the Recycle Bin".into()) } else { Ok(()) }
}

static SUM_CALLS: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
fn recycle_sum(files: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    SUM_CALLS.lock().unwrap().push(names_of(files));
    Ok(())
}

#[test]
fn companions_are_summed_with_the_clip_for_the_bin_check() {
    // Clip A is 1 GiB with 3 companions of 600 MiB each: 2.76 GiB, more than the 2 GiB of room.
    let out = run_once(Path::new(r"E:\Clips"), 500, plan_three, (&|| true, no_keeps), (two_gib_room, big_companions), recycle_sum).unwrap();
    assert!(SUM_CALLS.lock().unwrap().is_empty(), "nothing sent: {out:?}");
    assert!(out.refused.as_deref().is_some_and(|r| r.contains("no room")), "{out:?}");
}

static UNREADABLE_CALLS: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
fn recycle_unreadable(files: &[PathBuf]) -> Result<(), crate::record_storage::RecycleError> {
    UNREADABLE_CALLS.lock().unwrap().push(names_of(files));
    Ok(())
}

#[test]
fn a_companion_whose_size_cannot_be_read_refuses() {
    let out = run_once(Path::new(r"E:\Clips"), 500, plan_three, (&|| true, no_keeps), (any_bin, unreadable), recycle_unreadable).unwrap();
    assert!(UNREADABLE_CALLS.lock().unwrap().is_empty(), "nothing sent: {out:?}");
    assert!(out.refused.as_deref().is_some_and(|r| r.contains("couldn't read the size")), "{out:?}");
}
