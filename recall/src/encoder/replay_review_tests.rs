//! Restore policy review (P1, P2-2..P2-6), replay side, fail-first: a save killed during its
//! trailer is rebuilt (never renamed while unplayable), a save on a missing drive stays listed, a
//! rebuild that failed is retried, the cut `.part` is kept when it holds more than its index,
//! index files are only ever Recall's own, a slot is claimed only by its own save, and paths
//! compare the same with either slash. Plus the real Recycle Bin precheck (`win_recycle`, no bin
//! touched). Real libx264 + AAC through the production mux (CPU only), temp folders, a temp
//! app-data root, the fake Recycle Bin.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::super::in_progress::{self, Entry};
use super::super::mux::produce;
use super::super::mux::tests::window_job;
use super::super::part_recover::{layout, Layout};
use super::super::recycle::{self, fake};
use super::super::replay_index::{self, claim, idx_of, slot};
use super::super::saver::{window_secs, Timing};
use super::super::test_scratch::{scratch, Scratch};
use super::*;

struct Sandbox {
    _root: crate::user_dirs::test_root::Injected,
    dir: Scratch,
}

fn sandbox(tag: &str) -> Sandbox {
    let dir = scratch("recall-replay-review", tag);
    let root = crate::user_dirs::inject(&dir.join("app-root"));
    Sandbox { _root: root, dir }
}

/// A finished production save (`produce`) into `part`, its index at this thread's slot.
fn save_into(part: &Path, frames: usize) -> Option<()> {
    let (packets, params) = super::super::replay_index::tests::window(frames)?;
    let secs = window_secs(&packets);
    let clip = part.to_string_lossy().trim_end_matches(".part").to_string();
    produce(part, &window_job(&clip, packets, params, secs), &AtomicBool::new(false), &mut Timing::default()).expect("save");
    Some(())
}

fn moov_at(part: &Path) -> usize {
    let bytes = std::fs::read(part).unwrap();
    bytes.windows(4).rposition(|w| w == b"moov").unwrap() - 4
}

fn truncate(part: &Path, at: usize) {
    std::fs::OpenOptions::new().write(true).open(part).unwrap().set_len(at as u64).unwrap();
}

fn video_packets(path: &Path) -> usize {
    let Ok(mut input) = ffmpeg_next::format::input(&path) else { return 0 };
    input.packets().filter(|(s, _)| s.index() == 0).count()
}

/// A cut-off listed save (cut at `fraction` of its mdat) with a claimed index: (part, clip, entry).
fn listed_cut(sb: &Sandbox, name: &str, fraction: f64) -> Option<(PathBuf, PathBuf, Entry)> {
    let part = sb.dir.join(format!("{name}.mp4.part"));
    save_into(&part, 120)?;
    truncate(&part, (moov_at(&part) as f64 * fraction) as usize);
    let index = claim(&slot().unwrap()).unwrap();
    let clip = sb.dir.join(format!("{name}.mp4"));
    let entry = Entry::replay(&part, &clip, Some(&index));
    in_progress::add(entry.clone()).unwrap();
    Some((part, clip, entry))
}

/// P1: a kill during the trailer leaves a torn `moov`: not "finished"; rebuilt from its index.
#[test]
fn a_save_killed_during_its_trailer_is_rebuilt_not_renamed() {
    let sb = sandbox("torn-moov");
    let part = sb.dir.join("A__2026-11-11__10-00-00-000__aa.mp4.part");
    let Some(()) = save_into(&part, 120) else { return eprintln!("libx264 missing; skipped") };
    truncate(&part, moov_at(&part) + 40);
    assert_ne!(layout(&part).unwrap(), Layout::Finished, "a torn moov is not a finished file");
    let index = claim(&slot().unwrap()).unwrap();
    let clip = sb.dir.join("A__2026-11-11__10-00-00-000__aa.mp4");
    let entry = Entry::replay(&part, &clip, Some(&index));
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    rebuild_or_adopt(&entry, &mut clips, &mut logs);
    assert_eq!(clips, vec![clip.clone()], "{logs:?}");
    assert!(video_packets(&clip) > 0, "the clip in the library plays: {logs:?}");
}

fn rebuild_or_adopt(entry: &Entry, clips: &mut Vec<PathBuf>, logs: &mut Vec<String>) {
    recover_entry(entry, clips, logs);
}

/// P2-2: a listed save whose folder is gone (its drive not mounted) stays listed, index kept.
#[test]
fn a_listed_save_on_a_missing_drive_stays_listed() {
    let sb = sandbox("missing-drive");
    let part = sb.dir.join("not mounted").join("B__2026-11-11__10-00-00-000__bb.mp4.part");
    let base = PathBuf::from(format!("{}-claimed", slot().unwrap().display()));
    for f in [idx_of(&base), replay_index::hdr_of(&base)] {
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, b"index").unwrap();
    }
    let entry = Entry::replay(&part, &part.with_extension(""), Some(&base));
    in_progress::add(entry.clone()).unwrap();
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    recover_entry(&entry, &mut clips, &mut logs);
    assert!(in_progress::get(&part).is_some(), "kept for when its drive is back: {logs:?}");
    assert!(idx_of(&base).exists(), "its index too");
}

/// P2-3: a rebuild that fails for now (its clip folder cannot be made) leaves the save listed.
#[test]
fn a_failed_rebuild_is_retried_at_the_next_start() {
    let sb = sandbox("retry");
    let Some((part, _, entry)) = listed_cut(&sb, "C__2026-11-11__10-00-00-000__cc", 0.7) else { return eprintln!("libx264 missing; skipped") };
    std::fs::write(sb.dir.join("blocked"), b"a file where the folder should be").unwrap();
    let entry = Entry { clip: Some(sb.dir.join("blocked").join("C.mp4").to_string_lossy().into_owned()), ..entry };
    in_progress::add(entry.clone()).unwrap();
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    recover_entry(&entry, &mut clips, &mut logs);
    assert!(clips.is_empty() && part.exists(), "{logs:?}");
    assert!(in_progress::get(&part).is_some(), "still listed: the next start tries again");
}

/// P2-4: an index that lags its `.part` (a power cut kept more of the mux than of the index):
/// the clip is rebuilt from what the index knows, and the `.part` that holds more is kept.
#[test]
fn a_part_that_holds_more_than_its_index_is_kept() {
    let sb = sandbox("index-lags");
    let Some((part, clip, entry)) = listed_cut(&sb, "D__2026-11-11__10-00-00-000__dd", 0.9) else { return eprintln!("libx264 missing; skipped") };
    let idx = idx_of(Path::new(entry.index.as_deref().unwrap()));
    let bytes = std::fs::read(&idx).unwrap();
    let head = 12 + u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let records = (bytes.len() - head) / 32;
    std::fs::write(&idx, &bytes[..head + records / 2 * 32]).unwrap();
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    recover_entry(&entry, &mut clips, &mut logs);
    assert_eq!(clips, vec![clip.clone()], "{logs:?}");
    assert!(part.exists() && !fake::binned_under(&sb.dir).contains(&part), "the .part holds more than the clip: kept: {logs:?}");
}

/// P2-5: index files outside Recall's replay-index folder and a path that is not a replay save
/// are never touched; the entry is dropped.
#[test]
fn an_entry_that_is_not_recalls_is_never_touched() {
    let sb = sandbox("not-ours");
    let foreign = sb.dir.join("elsewhere").join("x");
    std::fs::create_dir_all(foreign.parent().unwrap()).unwrap();
    for f in [idx_of(&foreign), replay_index::hdr_of(&foreign)] {
        std::fs::write(f, b"not an index").unwrap();
    }
    let part = sb.dir.join("E__2026-11-11__10-00-00-000__ee.mp4.part");
    let clip = sb.dir.join("E__2026-11-11__10-00-00-000__ee.mp4");
    std::fs::write(&clip, b"saved").unwrap();
    let movie = sb.dir.join("movie.mkv");
    std::fs::write(&movie, b"the user's movie").unwrap();
    let entries = [Entry::replay(&part, &clip, Some(&foreign)), Entry::replay(&movie, &movie, None)];
    for e in &entries {
        in_progress::add(e.clone()).unwrap();
    }
    let (mut clips, mut logs) = (Vec::new(), Vec::new());
    for e in &entries {
        recover_entry(e, &mut clips, &mut logs);
    }
    assert!(idx_of(&foreign).exists() && replay_index::hdr_of(&foreign).exists(), "{logs:?}");
    assert_eq!(std::fs::read(&movie).unwrap(), b"the user's movie");
    assert!(fake::binned_under(&sb.dir).is_empty() && in_progress::list().is_empty(), "{logs:?}");
}

/// P2-6: the slot is claimed only by the save whose index it holds; paths compare the same
/// with either slash (a `RECALL_OUT_DIR` written with `/`).
#[test]
fn a_slot_is_claimed_only_by_its_own_save() {
    let sb = sandbox("claim");
    let theirs = sb.dir.join("F__2026-11-11__10-00-00-000__ff.mp4.part");
    let Some(()) = save_into(&theirs, 30) else { return eprintln!("libx264 missing; skipped") };
    let mine = sb.dir.join("G__2026-11-11__10-00-00-000__gg.mp4.part");
    let stale = Entry::replay(&mine, &mine.with_extension(""), Some(&slot().unwrap()));
    let claimed = crate::longrec::recover::claim_replay_slots(vec![stale], &mut Vec::new());
    assert!(idx_of(&slot().unwrap()).exists(), "another save's index stays in the slot");
    assert_eq!(claimed[0].index.as_deref().map(PathBuf::from), slot(), "not handed to the stale entry");
    let slashed = PathBuf::from(theirs.to_string_lossy().replace('\\', "/"));
    let ours = Entry::replay(&slashed, &slashed.with_extension(""), Some(&slot().unwrap()));
    let claimed = crate::longrec::recover::claim_replay_slots(vec![ours], &mut Vec::new());
    assert_ne!(claimed[0].index.as_deref().map(PathBuf::from), slot(), "its own save claims it, whatever the slashes");
}

/// The real Recycle Bin precheck (`win_recycle::can_recycle` on this volume, nothing moved) and
/// how an operation's outcome is read; only the move itself is faked in unit tests.
#[test]
fn the_real_recycle_bin_precheck_runs() {
    let sb = sandbox("precheck");
    let file = sb.dir.join("small.bin");
    std::fs::write(&file, b"x").unwrap();
    match recycle::precheck(&file) {
        Ok(()) => {}
        Err(why) => assert!(!why.is_empty(), "a refusal says why"),
    }
    assert!(file.exists(), "the precheck moves nothing");
    assert!(recycle::precheck(&sb.dir.join("missing.bin")).is_err_and(|e| e.contains("read")));
    assert_eq!(recycle::outcome(crate::win_fileop::Outcome::Recycled), Ok(()));
    assert_eq!(recycle::outcome(crate::win_fileop::Outcome::Refused("no".into())), Err("no".into()));
    assert_eq!(recycle::outcome(crate::win_fileop::Outcome::Failed("x".into())), Err("x".into()));
}
