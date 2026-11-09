use std::time::{Duration, Instant, SystemTime};

use super::*;

/// Deleted when the test ends, panics included (`TempDir`).
fn scratch(name: &str) -> crate::test_temp::TempDir {
    crate::test_temp::TempDir::new(format!("recall-fill-{}-{name}", std::process::id()))
}

fn pattern(bytes: usize) -> Vec<u8> {
    (0..bytes).map(|i| (i / 4096 % 251) as u8 ^ (i % 7) as u8).collect()
}

fn clip(dir: &Path, bytes: usize) -> (PathBuf, SystemTime) {
    let path = dir.join("clip.mp4");
    std::fs::write(&path, pattern(bytes)).unwrap();
    let modified = path.metadata().unwrap().modified().unwrap();
    (path, modified)
}

fn fill_at(dir: &Path, source: &Path, part: &Path, bytes: u64, modified: SystemTime) -> ClipFill {
    ClipFill::create(source, part, &dir.join("copy.mp4"), bytes, modified, 0, 0).unwrap()
}

fn fill(dir: &Path, source: &Path, bytes: u64, modified: SystemTime, throttle: u64) -> ClipFill {
    ClipFill::create(source, &dir.join("clip.part"), &dir.join("copy.mp4"), bytes, modified, throttle, 0).unwrap()
}

fn read_all(fill: &ClipFill, offset: u64, length: usize) -> Vec<u8> {
    let file = fill.open_reader().unwrap();
    let mut scratch = AlignedBuffer::new(1 << 20);
    let mut out = vec![0u8; length];
    let mut done = 0;
    while done < length {
        let window = (length - done).min((1 << 20) - 8192);
        let read = fill.read_at(&file, offset + done as u64, &mut out[done..done + window], &mut scratch).unwrap();
        assert!(read > 0);
        done += read;
    }
    out
}

#[test]
fn the_copy_arrives_whole_and_lands_under_its_cache_name() {
    let dir = scratch("whole");
    let bytes = 3 * CHUNK as usize + 12_345;
    let (source, modified) = clip(&dir, bytes);
    let fill = fill(&dir, &source, bytes as u64, modified, 0);
    fill.start(&[3, 0]).unwrap();
    assert!(fill.wait_done());
    assert_eq!(fill.outcome(), Outcome::Done);
    assert_eq!(fill.progress(), 1.0);
    assert!(fill.holes().is_empty());
    let copy = dir.join("copy.mp4");
    assert_eq!(std::fs::read(&copy).unwrap(), pattern(bytes));
    assert_eq!(copy.metadata().unwrap().modified().unwrap(), modified);
    assert!(!dir.join("clip.part").exists());
}

#[test]
fn a_read_past_the_copy_moves_it_and_gets_real_bytes() {
    let dir = scratch("jump");
    let bytes = 40 * CHUNK as usize;
    let (source, modified) = clip(&dir, bytes);
    // 40 MiB/s: 4 MiB takes 100 ms, so the far end is seconds away in file order.
    let fill = fill(&dir, &source, bytes as u64, modified, 40 << 20);
    fill.start(&[0]).unwrap();
    assert!(fill.wait_for(0, 1, true));
    let far = 30 * CHUNK + 1000;
    let started = std::time::Instant::now();
    assert!(fill.wait_for(far, far + 5000, true));
    assert!(started.elapsed() < Duration::from_secs(1), "took {:?}", started.elapsed());
    assert_eq!(read_all(&fill, far, 5000), pattern(bytes)[far as usize..far as usize + 5000]);
    assert!(fill.has(far, far + 1));
    assert!(!fill.has(10 * CHUNK, 11 * CHUNK), "the copy jumped over this part");
    let holes = fill.holes();
    assert!(holes.first().is_some_and(|hole| hole.0 < far), "{holes:?}");
    assert!(fill.wait_done(), "the copy wraps round to what it skipped");
}

#[test]
fn cancelling_frees_waiting_readers_and_leaves_no_copy() {
    let dir = scratch("cancel");
    let bytes = 20 * CHUNK as usize;
    let (source, modified) = clip(&dir, bytes);
    let fill = fill(&dir, &source, bytes as u64, modified, 1 << 20);
    fill.start(&[0]).unwrap();
    let waiting = {
        let fill = fill.clone();
        std::thread::spawn(move || fill.wait_for(19 * CHUNK, 19 * CHUNK + 1, false))
    };
    std::thread::sleep(Duration::from_millis(50));
    fill.cancel();
    assert!(!waiting.join().unwrap());
    assert!(!fill.wait_done());
    assert!(!dir.join("copy.mp4").exists());
}

#[test]
fn a_clip_that_changes_while_copying_is_not_published() {
    let dir = scratch("changed");
    let bytes = 2 * CHUNK as usize;
    let (source, modified) = clip(&dir, bytes);
    let fill = fill(&dir, &source, bytes as u64, modified - Duration::from_secs(60), 0);
    fill.start(&[0]).unwrap();
    assert!(!fill.wait_done());
    assert!(fill.error().is_some_and(|error| error.contains("changed")));
    assert!(!dir.join("copy.mp4").exists());
}

#[test]
fn a_drive_that_stops_answering_fails_the_fill_and_frees_its_readers() {
    let dir = scratch("failed");
    let bytes = 8 * CHUNK as usize;
    let (source, modified) = clip(&dir, bytes);
    let fill = ClipFill::create(&source, &dir.join("clip.part"), &dir.join("copy.mp4"), bytes as u64, modified, 0, 3 * CHUNK).unwrap();
    fill.start(&[0]).unwrap();
    assert!(!fill.wait_done());
    assert_eq!(fill.outcome(), Outcome::Failed);
    assert!(fill.error().is_some());
    assert!(fill.has(0, CHUNK), "what arrived stays readable");
    let hole = fill.holes()[0];
    assert!(!fill.wait_for(hole.0, hole.0 + 1, true), "a reader on a missing part is freed");
    assert!(!fill.stays_open(), "before the player has it, the open fails");
    fill.set_playing();
    assert!(fill.stays_open());
    let waiting = {
        let fill = fill.clone();
        std::thread::spawn(move || fill.wait_released())
    };
    fill.cancel();
    assert!(!waiting.join().unwrap(), "closing the clip frees it");
    assert!(!fill.stays_open());
    assert!(!dir.join("copy.mp4").exists());
}

#[test]
fn a_read_stopped_on_a_hole_lets_go_for_a_seek_only() {
    let dir = scratch("released");
    let bytes = 8 * CHUNK as usize;
    let (source, modified) = clip(&dir, bytes);
    let fill = ClipFill::create(&source, &dir.join("clip.part"), &dir.join("copy.mp4"), bytes as u64, modified, 0, 3 * CHUNK).unwrap();
    fill.start(&[0]).unwrap();
    assert!(!fill.wait_done());
    fill.set_playing();
    let waiting = {
        let fill = fill.clone();
        std::thread::spawn(move || fill.wait_released())
    };
    std::thread::sleep(Duration::from_millis(200));
    assert!(!waiting.is_finished(), "nothing to do until the player seeks");
    fill.note_seek();
    assert!(waiting.join().unwrap(), "a seek gets the queue back");
    std::thread::sleep(RELEASE + Duration::from_millis(100));
    let later = {
        let fill = fill.clone();
        std::thread::spawn(move || fill.wait_released())
    };
    std::thread::sleep(Duration::from_millis(200));
    assert!(!later.is_finished(), "long after the seek reads wait again");
    fill.cancel();
    assert!(!later.join().unwrap());
}

#[test]
fn a_clip_read_where_it_lies_says_when_its_drive_goes() {
    let dir = scratch("direct-gone");
    let bytes = 4 * CHUNK as usize;
    let (source, modified) = clip(&dir, bytes);
    let fill = ClipFill::direct(&source, bytes as u64, modified, 2 * CHUNK);
    fill.set_playing();
    fill.note_seek();
    let file = fill.open_reader().unwrap();
    let mut scratch = AlignedBuffer::new(1 << 20);
    let mut out = vec![0u8; 4096];
    assert!(fill.read_at(&file, 0, &mut out, &mut scratch).is_ok());
    assert!(fill.read_at(&file, 2 * CHUNK, &mut out, &mut scratch).is_err());
    assert_eq!(fill.outcome(), Outcome::Done, "the fill has no reader of its own to notice");
    fill.lost("gone".into());
    assert_eq!(fill.outcome(), Outcome::Failed);
    assert_eq!(fill.error().as_deref(), Some("gone"));
    assert!(!fill.has(0, 1), "nothing counts as there any more");
    assert!(!fill.wait_for(0, 1, true));
    assert!(fill.stays_open());
    fill.cancel();
    drop(fill);
    assert_eq!(std::fs::read(&source).unwrap(), pattern(bytes), "the clip is left alone");
}

#[test]
fn only_a_clip_read_where_it_lies_can_be_lost_this_way() {
    let dir = scratch("copy-not-lost");
    let (source, modified) = clip(&dir, CHUNK as usize);
    let fill = fill(&dir, &source, CHUNK, modified, 0);
    fill.lost("gone".into());
    assert_eq!(fill.outcome(), Outcome::Filling);
}

#[test]
fn paced_copying_never_stops_and_a_stopped_reader_is_served_at_once() {
    // While recording (or with the editor hidden) the open clip is paced: it still loads fully.
    let dir = scratch("paced");
    let bytes = 12 * CHUNK as usize;
    let (source, modified) = clip(&dir, bytes);
    let fill = fill(&dir, &source, bytes as u64, modified, 0);
    fill.pace(true);
    fill.start(&[0]).unwrap();
    assert!(fill.wait_for(0, 1, true));
    let asked = Instant::now();
    assert!(fill.wait_for(10 * CHUNK, 10 * CHUNK + 1, true), "a stopped reader gets its part");
    assert!(asked.elapsed() < Duration::from_millis(600), "its part is not paced: {:?}", asked.elapsed());
    assert!(fill.wait_done(), "a paced fill finishes on its own");
    assert_eq!(fill.outcome(), Outcome::Done);
    assert_eq!(std::fs::read(fill.target()).unwrap(), pattern(bytes));
}

#[test]
fn paced_copying_keeps_to_its_rate() {
    let dir = scratch("paced-rate");
    let chunks = 8u64;
    let bytes = (chunks * CHUNK) as usize;
    let (source, modified) = clip(&dir, bytes);
    let paced = fill(&dir, &source, bytes as u64, modified, 0);
    paced.pace(true);
    let started = Instant::now();
    paced.start(&[0]).unwrap();
    assert!(paced.wait_done());
    let took = started.elapsed().as_secs_f64();
    // The first chunk is asked for (full speed); the other 7 go at the cap.
    let floor = (chunks - 1) as f64 * CHUNK as f64 / PACED_RATE as f64;
    assert!(took >= floor * 0.9, "{took:.2}s for {:.1} MB is faster than {} MB/s", bytes as f64 / 1e6, PACED_RATE / 1_000_000);
    assert!(took < floor * 3.0 + 1.0, "{took:.2}s: pacing slower than the cap");
    // Released, the same copy goes at the drive's speed.
    let dir = scratch("unpaced-rate");
    let (source, modified) = clip(&dir, bytes);
    let fast = fill(&dir, &source, bytes as u64, modified, 0);
    let started = Instant::now();
    fast.start(&[0]).unwrap();
    assert!(fast.wait_done());
    assert!(started.elapsed().as_secs_f64() < floor, "unpaced is not capped");
}

#[test]
fn a_clip_read_where_it_lies_is_never_touched() {
    let dir = scratch("direct");
    let bytes = 2 * CHUNK as usize + 100;
    let (source, modified) = clip(&dir, bytes);
    let fill = ClipFill::direct(&source, bytes as u64, modified, 0);
    assert_eq!(fill.outcome(), Outcome::Done);
    assert!(fill.has(0, bytes as u64));
    assert!(fill.wait_for(0, bytes as u64, true));
    assert_eq!(read_all(&fill, CHUNK + 5, 4000), pattern(bytes)[CHUNK as usize + 5..CHUNK as usize + 4005]);
    fill.cancel();
    drop(fill);
    assert_eq!(std::fs::read(&source).unwrap(), pattern(bytes), "dropping the fill leaves the clip");
}

#[test]
fn parts_in_use_are_known_until_the_fill_goes() {
    let dir = scratch("in-use");
    let bytes = CHUNK as usize;
    let (source, modified) = clip(&dir, bytes);
    let fill = fill(&dir, &source, bytes as u64, modified, 0);
    let part = dir.join("clip.part");
    assert!(in_use(&part));
    drop(fill);
    assert!(!in_use(&part));
    assert!(!part.exists());
}

#[test]
fn a_fill_only_ever_removes_its_own_part_file() {
    let dir = scratch("own-part");
    let bytes = 20 * CHUNK as usize;
    let (source, modified) = clip(&dir, bytes);
    let make = |name: &str| ClipFill::create(&source, &dir.join(name), &dir.join(name.replace(".part", "")), bytes as u64, modified, 40 << 20, 0).unwrap();
    let (stale, current) = (make("a.mp4.1-1.part"), make("b.mp4.1-2.part"));
    stale.start(&[0]).unwrap();
    current.start(&[0]).unwrap();
    stale.cancel();
    assert!(!stale.wait_done());
    drop(stale);
    let gone = std::time::Instant::now();
    while dir.join("a.mp4.1-1.part").exists() && gone.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!dir.join("a.mp4.1-1.part").exists(), "the stale fill's own part goes");
    assert!(dir.join("b.mp4.1-2.part").exists(), "the current clip's part stays");
    assert_eq!(current.outcome(), Outcome::Filling, "and the current fill carries on");
    current.cancel();
}

#[test]
fn chunks_of_a_range_stay_inside_the_clip() {
    assert_eq!(spanning(0, 1, 10).collect::<Vec<_>>(), [0]);
    assert_eq!(spanning(CHUNK - 1, CHUNK + 1, 3 * CHUNK).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(spanning(10 * CHUNK, 20 * CHUNK, 3 * CHUNK).collect::<Vec<_>>(), [2]);
    assert_eq!(spanning(CHUNK, CHUNK, 3 * CHUNK).collect::<Vec<_>>(), [1]);
}

#[test]
fn a_seeded_fill_has_its_head_at_once_and_copies_only_the_rest() {
    let dir = scratch("seeded");
    let bytes = 6 * CHUNK as usize;
    let (source, modified) = clip(&dir, bytes);
    // A head from the filter probe: chunks 0, 1 and 5 already in place.
    let part = dir.join("clip.part");
    let data = pattern(bytes);
    let mut head = vec![0u8; bytes];
    for chunk in [0usize, 1, 5] {
        let range = chunk * CHUNK as usize..(chunk + 1) * CHUNK as usize;
        head[range.clone()].copy_from_slice(&data[range]);
    }
    std::fs::write(&part, &head).unwrap();
    let fill = ClipFill::create_seeded(&source, &part, &dir.join("copy.mp4"), bytes as u64, modified, &[0, 1, 5], 0, 0).unwrap();
    assert!(fill.has(0, 2 * CHUNK) && fill.has(5 * CHUNK, 6 * CHUNK), "the head plays before the drive is read");
    assert!(!fill.has(2 * CHUNK, 3 * CHUNK));
    assert!((fill.progress() - 0.5).abs() < 1e-9, "{}", fill.progress());
    fill.start(&[5, 0]).unwrap();
    assert!(fill.wait_done());
    assert_eq!(std::fs::read(dir.join("copy.mp4")).unwrap(), data, "the rest arrives around the head");
}

#[test]
fn a_reader_opened_after_the_copy_finished_reads_the_published_file() {
    // The sound decoder or a swapped source can open the fill after its .part was renamed.
    let dir = scratch("late-reader");
    let bytes = 2 * CHUNK as usize + 999;
    let (source, modified) = clip(&dir, bytes);
    let fill = fill(&dir, &source, bytes as u64, modified, 0);
    fill.start(&[0]).unwrap();
    assert!(fill.wait_done());
    assert!(!dir.join("clip.part").exists(), "published under its cache name");
    assert_eq!(read_all(&fill, CHUNK + 3, 5000), pattern(bytes)[CHUNK as usize + 3..CHUNK as usize + 5003]);
}

#[test]
fn a_fill_owns_its_part_before_the_file_exists() {
    let dir = scratch("owned-early");
    let bytes = CHUNK as usize;
    let (source, modified) = clip(&dir, bytes);
    let part = dir.join("Clip.PART");
    let fill = fill_at(&dir, &source, &part, bytes as u64, modified);
    assert!(super::in_use(&dir.join("clip.part")), "owned, whatever the case of the name");
    drop(fill);
    assert!(!super::in_use(&part), "released with the fill");
}


#[test]
fn a_direct_fill_never_opens_its_source_for_write() {
    // A direct fill's part IS the source clip: start() opened it for write and finish() would
    // set_len it (here 100 bytes short of the file) and stamp it.
    let dir = scratch("direct-write");
    let bytes = CHUNK as usize + 5000;
    let (source, modified) = clip(&dir, bytes);
    let fill = ClipFill::direct(&source, bytes as u64 - 100, modified, 0);
    assert!(fill.start(&[0]).is_err(), "start opened the source clip for write");
    let (input, output) = (std::fs::File::open(&source).unwrap(), std::fs::OpenOptions::new().write(true).open(&source).unwrap());
    let mut reader = reader::Reader::new(input, output);
    assert!(fill.finish(&mut reader).is_err(), "finish ran on the source clip");
    drop(reader);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(std::fs::read(&source).unwrap(), pattern(bytes), "the source clip is untouched");
    assert_eq!(source.metadata().unwrap().modified().unwrap(), modified);
}
