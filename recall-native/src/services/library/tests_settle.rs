//! Folder watcher: the settle rules on a fake clock.
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use super::settle::{BATCH_GAP, BUSY_MAX, EMPTY_GRACE, Kind, MAX_PROBES, PASS_GAP, Probe, QUIET, RECHECK, Settler};

const READY: Probe = Probe::Ready { bytes: 10, mtime: 7 };

fn clip(name: &str) -> PathBuf {
    PathBuf::from(r"D:\c").join(name)
}

fn check(settler: &mut Settler, path: &Path, result: Probe, now: Instant) {
    let due = settler.due(now);
    assert!(due.iter().any(|(p, _)| p == path), "{} due at this time", path.display());
    settler.report(path, result, now);
}

#[test]
fn a_file_is_not_looked_at_until_it_has_been_quiet() {
    let (mut settler, t0) = (Settler::new(), Instant::now());
    settler.note(clip("a.mp4"), Kind::Clip, t0);
    assert!(settler.due(t0 + QUIET - Duration::from_millis(1)).is_empty());
    // More writes push the check back.
    settler.note(clip("a.mp4"), Kind::Clip, t0 + QUIET / 2);
    assert!(settler.due(t0 + QUIET).is_empty());
    assert_eq!(settler.due(t0 + QUIET / 2 + QUIET).len(), 1);
}

#[test]
fn a_clip_settles_after_two_checks_that_agree() {
    let (mut settler, t0) = (Settler::new(), Instant::now());
    let path = clip("a.mp4");
    settler.note(path.clone(), Kind::Clip, t0);
    let t1 = t0 + QUIET;
    check(&mut settler, &path, READY, t1);
    assert!(settler.take_batch(t1).is_none(), "one check is not enough");
    assert!(settler.due(t1 + RECHECK - Duration::from_millis(1)).is_empty());
    let t2 = t1 + RECHECK;
    check(&mut settler, &path, READY, t2);
    let batch = settler.take_batch(t2).expect("settled");
    assert_eq!(batch.ready, [(path, 10, 7)]);
    assert!(settler.next_wake(t2).is_none(), "nothing left to do");
}

#[test]
fn a_clip_that_is_still_growing_waits() {
    let (mut settler, t0) = (Settler::new(), Instant::now());
    let path = clip("a.mp4");
    settler.note(path.clone(), Kind::Clip, t0);
    let t1 = t0 + QUIET;
    check(&mut settler, &path, Probe::Ready { bytes: 10, mtime: 7 }, t1);
    check(&mut settler, &path, Probe::Ready { bytes: 20, mtime: 7 }, t1 + RECHECK);
    assert!(settler.take_batch(t1 + RECHECK).is_none(), "size moved");
    check(&mut settler, &path, Probe::Ready { bytes: 20, mtime: 7 }, t1 + RECHECK * 2);
    assert_eq!(settler.take_batch(t1 + RECHECK * 2).unwrap().ready[0].1, 20);
}

#[test]
fn a_file_held_open_never_settles_and_is_rechecked_at_most_a_second_apart() {
    let (mut settler, t0) = (Settler::new(), Instant::now());
    let path = clip("a.mp4");
    settler.note(path.clone(), Kind::Clip, t0);
    let mut now = t0 + QUIET;
    for _ in 0..20 {
        check(&mut settler, &path, Probe::Busy, now);
        assert!(settler.take_batch(now).is_none());
        let wait = settler.next_wake(now).unwrap();
        assert!(wait <= BUSY_MAX, "{wait:?}");
        now += wait;
    }
    // Writer closed: two agreeing checks after that, under the 3 s a tile may take.
    let closed = now;
    check(&mut settler, &path, READY, now);
    now += settler.next_wake(now).unwrap();
    check(&mut settler, &path, READY, now);
    assert!(settler.take_batch(now).is_some());
    assert!(now.duration_since(closed) + BUSY_MAX < Duration::from_secs(3));
}

#[test]
fn an_empty_clip_waits_out_its_grace_then_counts() {
    let (mut settler, t0) = (Settler::new(), Instant::now());
    let path = clip("a.mp4");
    let empty = Probe::Ready { bytes: 0, mtime: 7 };
    settler.note(path.clone(), Kind::Clip, t0);
    let mut now = t0 + QUIET;
    while now.duration_since(t0) < EMPTY_GRACE {
        check(&mut settler, &path, empty, now);
        now += settler.next_wake(now).unwrap();
    }
    assert!(settler.take_batch(now).is_none());
    check(&mut settler, &path, empty, now);
    now += settler.next_wake(now).unwrap();
    check(&mut settler, &path, empty, now);
    assert_eq!(settler.take_batch(now).unwrap().ready.len(), 1);
}

#[test]
fn a_rename_is_one_leaving_and_one_arriving() {
    let (mut settler, t0) = (Settler::new(), Instant::now());
    let (old, new) = (clip("a.mp4"), clip("b.mp4"));
    settler.note(old.clone(), Kind::Clip, t0);
    settler.note(new.clone(), Kind::Clip, t0);
    let t1 = t0 + QUIET;
    settler.report(&old, Probe::Missing, t1);
    settler.report(&new, READY, t1);
    settler.report(&new, READY, t1 + RECHECK);
    let batch = settler.take_batch(t1 + RECHECK).unwrap();
    assert_eq!((batch.gone, batch.ready.len()), (vec![old], 1));
}

#[test]
fn a_file_replaced_in_place_does_not_flicker_out() {
    let (mut settler, t0) = (Settler::new(), Instant::now());
    let path = clip("a.mp4");
    settler.note(path.clone(), Kind::Clip, t0); // removed
    settler.note(path.clone(), Kind::Clip, t0 + QUIET / 2); // and back, before anyone looked
    let t1 = t0 + QUIET / 2 + QUIET;
    check(&mut settler, &path, READY, t1);
    check(&mut settler, &path, READY, t1 + RECHECK);
    let batch = settler.take_batch(t1 + RECHECK).unwrap();
    assert!(batch.gone.is_empty());
    assert_eq!(batch.ready.len(), 1);
}

#[test]
fn a_new_event_replaces_a_verdict_still_waiting_for_its_batch() {
    let (mut settler, t0) = (Settler::new(), Instant::now());
    let path = clip("a.mp4");
    settler.note(clip("first.mp4"), Kind::Clip, t0);
    settler.report(&clip("first.mp4"), Probe::Missing, t0);
    assert!(settler.take_batch(t0).is_some(), "opens the batch gate");
    settler.note(path.clone(), Kind::Clip, t0);
    settler.report(&path, Probe::Missing, t0);
    assert!(settler.take_batch(t0).is_none(), "too soon");
    settler.note(path, Kind::Clip, t0 + Duration::from_millis(10));
    assert!(settler.take_batch(t0 + BATCH_GAP).is_none(), "gone is no longer known");
}

#[test]
fn a_burst_is_checked_in_slices_and_leaves_in_batches() {
    let (mut settler, t0) = (Settler::new(), Instant::now());
    let total = MAX_PROBES * 3;
    for index in 0..total {
        settler.note(clip(&format!("{index}.mp4")), Kind::Clip, t0);
    }
    let (mut now, mut batches, mut ready, mut passes) = (t0 + QUIET, 0, 0, 0);
    while ready < total {
        let due = settler.due(now);
        assert!(due.len() <= MAX_PROBES);
        for (path, _) in due {
            settler.report(&path, READY, now);
        }
        if let Some(batch) = settler.take_batch(now) {
            batches += 1;
            ready += batch.ready.len();
        }
        passes += 1;
        now += settler.next_wake(now).unwrap_or(BATCH_GAP);
    }
    assert!(batches <= 4, "{total} files became {batches} list updates in {passes} passes");
    assert!(settler.next_wake(now).is_none());
    assert!(PASS_GAP <= BATCH_GAP);
}

#[test]
fn a_folder_that_arrives_is_walked_and_one_that_leaves_takes_its_clips() {
    let (mut settler, t0) = (Settler::new(), Instant::now());
    let (came, left, notes) = (clip("came"), clip("left"), clip("notes.txt"));
    settler.note(came.clone(), Kind::Other, t0);
    settler.note(left.clone(), Kind::Other, t0);
    settler.note(notes.clone(), Kind::Other, t0);
    let t1 = t0 + QUIET;
    settler.report(&came, Probe::Directory, t1);
    settler.report(&left, Probe::Missing, t1);
    settler.report(&notes, READY, t1);
    assert_eq!(settler.take_walks(), [came]);
    assert_eq!(settler.take_batch(t1).unwrap().gone_under, [left]);
    assert!(settler.next_wake(t1).is_none());
}

#[test]
fn a_lost_folder_stops_its_waiting_files() {
    let (mut settler, t0) = (Settler::new(), Instant::now());
    settler.note(clip("a.mp4"), Kind::Clip, t0);
    settler.note(PathBuf::from(r"E:\other\b.mp4"), Kind::Clip, t0);
    settler.forget_under(Path::new(r"D:\c"));
    assert_eq!(settler.due(t0 + QUIET).len(), 1);
}
