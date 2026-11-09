//! Filter probe: which clips, which chunks, the head file round trip, cancelling.
use super::*;

fn scratch(tag: &str) -> crate::test_temp::TempDir {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    crate::test_temp::TempDir::new(format!("recall-head-{tag}-{}-{nonce}", std::process::id()))
}

fn probe(name: &str, mtime_secs: u64) -> ProbeClip {
    ProbeClip { path: format!(r"E:\clips\{name}.mp4"), bytes: 1000, mtime_secs }
}

/// A recorder-shaped clip: `ftyp`, a patterned `mdat`, the index (`moov`) at the end.
fn recorder_clip(dir: &Path, bytes: usize) -> PathBuf {
    let moov = 64usize;
    let mdat = bytes - 16 - moov;
    let mut data = Vec::with_capacity(bytes);
    data.extend_from_slice(&16u32.to_be_bytes());
    data.extend_from_slice(b"ftypisom\0\0\0\0");
    data.extend_from_slice(&(mdat as u32).to_be_bytes());
    data.extend_from_slice(b"mdat");
    data.extend((0..mdat - 8).map(|i| (i % 251) as u8));
    data.extend_from_slice(&(moov as u32).to_be_bytes());
    data.extend_from_slice(b"moov");
    data.extend(std::iter::repeat_n(7u8, moov - 8));
    assert_eq!(data.len(), bytes);
    let path = dir.join("Rocket League__2026-11-02__10-00-00-000__ab.mp4");
    std::fs::write(&path, data).unwrap();
    path
}

#[test]
fn the_probe_takes_the_top_eight_by_score_then_newest() {
    let clips: Vec<ProbeClip> = (0..12).map(|i| probe(&format!("c{i}"), 1000 + i)).collect();
    // c3 scores highest; everything else ties, so the newest come next.
    let picked = pick(&clips, &|clip| if clip.path.ends_with("c3.mp4") { 5.0 } else { 1.0 });
    let names: Vec<&str> = picked.iter().map(|c| c.path.rsplit('\\').next().unwrap()).collect();
    assert_eq!(names, ["c3.mp4", "c11.mp4", "c10.mp4", "c9.mp4", "c8.mp4", "c7.mp4", "c6.mp4", "c5.mp4"]);
    assert_eq!(picked.len(), PROBE_TOP);
}

#[test]
fn a_head_is_the_index_plus_the_first_32_mb() {
    let bytes = 100 * CHUNK + 123;
    let index = (bytes - 3 * CHUNK, bytes);
    let chunks = head_chunks(bytes, index);
    let start: Vec<u64> = (0..HEAD_BYTES / CHUNK).collect();
    assert_eq!(&chunks[..start.len()], &start[..], "the start of the clip");
    assert_eq!(&chunks[start.len()..], &[97, 98, 99, 100], "and its index at the end");
    assert_eq!(head_chunks(CHUNK / 2, (100, 200)), vec![0], "a small clip is one chunk");
}

#[test]
fn a_head_seeds_the_fill_of_its_clip() {
    let (root, clips) = (scratch("root"), scratch("clips"));
    let bytes = 3 * CHUNK as usize + 4096;
    let source = recorder_clip(&clips, bytes);
    assert!(write_head(&root, &source, &|| true).unwrap());
    assert!(!write_head(&root, &source, &|| true).unwrap(), "one head per clip revision");

    let metadata = source.metadata().unwrap();
    let id = super::super::identity(&super::super::file_name(&source).unwrap(), metadata.len(), super::super::mtime_secs(&metadata));
    let part = root.join("fill.part");
    let chunks = take(&root, &id, metadata.len(), super::super::mtime_secs(&metadata), &part).expect("head taken");
    assert_eq!(chunks, vec![0, 1, 2, 3], "start and index cover this small clip");
    assert_eq!(std::fs::read(&part).unwrap(), std::fs::read(&source).unwrap(), "same bytes at the same offsets");
    assert!(take(&root, &id, metadata.len(), super::super::mtime_secs(&metadata), &part).is_none(), "taken once");
    let _ = (std::fs::remove_dir_all(&root), std::fs::remove_dir_all(&clips));
}

#[test]
fn a_cancelled_probe_writes_nothing_and_a_stale_head_is_dropped() {
    let (root, clips) = (scratch("root"), scratch("clips"));
    let source = recorder_clip(&clips, 2 * CHUNK as usize);
    assert!(!write_head(&root, &source, &|| false).unwrap(), "cancelled before the first chunk");
    assert!(std::fs::read_dir(heads_dir(&root)).map_or(true, |mut d| d.next().is_none()), "no head and no .part left");

    assert!(write_head(&root, &source, &|| true).unwrap());
    let metadata = source.metadata().unwrap();
    let id = super::super::identity(&super::super::file_name(&source).unwrap(), metadata.len(), super::super::mtime_secs(&metadata));
    let part = root.join("fill.part");
    assert!(take(&root, &id, metadata.len() + 1, super::super::mtime_secs(&metadata), &part).is_none(), "another revision");
    assert!(!head_paths(&root, &id).0.exists(), "a stale head is deleted");
    let _ = (std::fs::remove_dir_all(&root), std::fs::remove_dir_all(&clips));
}

#[test]
fn a_new_filter_cancels_the_queued_probe() {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    assert!(is_current(generation));
    cancel_probe();
    assert!(!is_current(generation), "queued heads of the old filter do nothing");
}
