use std::time::{Duration, SystemTime};

use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-fill-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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

fn fill(dir: &Path, source: &Path, bytes: u64, modified: SystemTime, throttle: u64) -> ClipFill {
    ClipFill::create(source, &dir.join("clip.part"), &dir.join("copy.mp4"), bytes, modified, throttle, false).unwrap()
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
fn chunks_of_a_range_stay_inside_the_clip() {
    assert_eq!(spanning(0, 1, 10).collect::<Vec<_>>(), [0]);
    assert_eq!(spanning(CHUNK - 1, CHUNK + 1, 3 * CHUNK).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(spanning(10 * CHUNK, 20 * CHUNK, 3 * CHUNK).collect::<Vec<_>>(), [2]);
    assert_eq!(spanning(CHUNK, CHUNK, 3 * CHUNK).collect::<Vec<_>>(), [1]);
}
