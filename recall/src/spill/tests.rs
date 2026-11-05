use super::drives::{rank, root_of, Drive};
use super::*;
use crate::ring::{EncodedPacket, PacketRing};

const GB: u64 = 1024 * MIB;

fn quiet() -> Arc<dyn Fn(&str) + Send + Sync> {
    Arc::new(|_: &str| {})
}

fn temp_file(tag: &str, cap: u64) -> Arc<SpillFile> {
    let path = std::env::temp_dir().join(format!("recall-spill-test-{tag}-{}", uuid::Uuid::new_v4().simple()));
    Arc::new(SpillFile::create(path, cap).expect("create spill file"))
}

#[test]
fn budget_is_an_eighth_of_ram_less_audio_with_a_floor() {
    let b = Budget { total: 32 * GB, audio: 92 * MIB, forced: None };
    assert_eq!(b.base(), 4 * GB - 92 * MIB);
    let small = Budget { total: 4 * GB, audio: 600 * MIB, forced: None };
    assert_eq!(small.base(), 128 * MIB, "never below 128 MiB");
    let forced = Budget { total: 32 * GB, audio: 0, forced: Some(10 * MIB) };
    assert_eq!(forced.base(), 10 * MIB);
}

#[test]
fn low_free_ram_shrinks_the_budget_for_the_game() {
    let b = Budget { total: 16 * GB, audio: 0, forced: None };
    assert_eq!(b.floor(), 16 * GB / 10, "10% of 16 GiB beats 1.5 GiB");
    assert_eq!(b.now(8 * GB, 1 * GB), b.base(), "plenty free: steady budget");
    // 500 MiB short of the floor: the ring gives that back.
    let short = b.floor() - 500 * MIB;
    assert_eq!(b.now(short, 1 * GB), 1 * GB - 500 * MIB);
    assert_eq!(b.now(0, 100 * MIB), 32 * MIB, "never below 32 MiB");
    let small = Budget { total: 8 * GB, audio: 0, forced: None };
    assert_eq!(small.floor(), 3 * GB / 2, "1.5 GiB beats 10% of 8 GiB");
}

fn drive(root: &str, disk: Option<u32>, ssd: Option<bool>, free_gb: u64, system: bool) -> Drive {
    Drive { root: root.into(), disk, ssd, free: free_gb * GB, system }
}

#[test]
fn spill_prefers_another_disk_with_room_ssd_first_and_the_games_disk_last() {
    let want = 2 * GB;
    let keep = KEEP_FREE;
    let game_ssd = drive("C:\\", Some(0), Some(true), 100, true);
    let other_ssd = drive("L:\\", Some(1), Some(true), 50, false);
    let hdd = drive("V:\\", Some(2), Some(false), 900, false);
    let all = [game_ssd.clone(), other_ssd.clone(), hdd.clone()];
    assert_eq!(rank(&all, Some(0), want, keep, MIN_FILE), Some((1, want)), "another SSD first");
    assert_eq!(rank(&[game_ssd.clone(), hdd.clone()], Some(0), want, keep, MIN_FILE), Some((1, want)), "then another HDD");
    assert_eq!(rank(&[game_ssd.clone()], Some(0), want, keep, MIN_FILE), Some((0, want)), "the game's disk last");
    // The game on L: makes C: the other SSD; the Windows drive still beats an HDD.
    assert_eq!(rank(&all, Some(1), want, keep, MIN_FILE), Some((0, want)));
}

#[test]
fn spill_never_eats_the_last_free_space() {
    let want = 2 * GB;
    let tight = drive("L:\\", Some(1), Some(true), 5, false);
    let roomy = drive("V:\\", Some(2), Some(false), 900, false);
    // L: has 1 GiB past KEEP_FREE: a partial file there loses to a whole one on V:.
    assert_eq!(rank(&[tight.clone(), roomy], Some(0), want, KEEP_FREE, MIN_FILE), Some((1, want)));
    assert_eq!(rank(&[tight.clone()], Some(0), want, KEEP_FREE, MIN_FILE), Some((0, 1 * GB)), "partial when it is all there is");
    let full = drive("L:\\", Some(1), Some(true), 4, false);
    assert_eq!(rank(&[full], Some(0), want, KEEP_FREE, MIN_FILE), None);
}

#[test]
fn drive_root_of_paths() {
    assert_eq!(root_of(std::path::Path::new(r"d:\Games\Valorant\VALORANT.exe")).as_deref(), Some("D:\\"));
    assert_eq!(root_of(std::path::Path::new(r"\\server\share\x.exe")), None);
}

#[test]
fn spill_file_wraps_and_reports_overwritten_bytes() {
    let file = temp_file("wrap", 10);
    let a = file.append(&[1, 2, 3, 4, 5, 6]).unwrap();
    let b = file.append(&[7, 8, 9, 10, 11, 12]).unwrap(); // wraps past the end
    assert_eq!(file.read(b, 6).unwrap(), vec![7, 8, 9, 10, 11, 12]);
    assert!(!file.holds(a), "the first packet was overwritten");
    assert!(file.read(a, 6).is_err());
    assert!(file.append(&[0; 11]).is_err(), "a packet larger than the file is refused");
}

#[test]
fn spilled_payload_reads_back_and_leaves_ram() {
    let gauge = Arc::new(AtomicUsize::new(0));
    let file = temp_file("move", 1 << 20);
    let p = Payload::from(vec![9u8; 1000]);
    let ring_handle = p.clone();
    p.track(&gauge);
    p.track(&gauge);
    assert_eq!(gauge.load(Relaxed), 1000, "tracked once");
    assert!(p.spill_to(&file).unwrap());
    assert!(!ring_handle.in_ram());
    assert_eq!(gauge.load(Relaxed), 0);
    assert_eq!(ring_handle.read().unwrap().as_ref(), &[9u8; 1000][..]);
    drop((p, ring_handle));
    assert_eq!(gauge.load(Relaxed), 0, "a spilled chunk does not uncount on drop");

    let q = Payload::from(vec![1u8; 10]);
    q.track(&gauge);
    drop(q);
    assert_eq!(gauge.load(Relaxed), 0, "a RAM chunk uncounts on drop");
}

fn packet(i: u64, len: usize) -> EncodedPacket {
    EncodedPacket { frame_idx: i * 16_666, duration_us: 16_666, keyframe: i % 60 == 0, data: Payload::from(vec![i as u8; len]) }
}

/// A spill whose file is a temp file (no drive probing) and whose budget is `budget`.
fn test_spill(budget: u64, file_cap: u64) -> Arc<Spill> {
    let spill = Spill::new(Budget { total: 0, audio: 0, forced: Some(budget) }, SpillPlan { want_file: file_cap }, quiet());
    let file = temp_file("ring", file_cap);
    let _ = spill.file.set(Arc::clone(&file));
    // Start the worker on the test file directly.
    let (tx, rx) = channel::<Payload>();
    let pending = Arc::clone(&spill.pending);
    std::thread::spawn(move || {
        for payload in rx {
            let len = payload.len();
            if payload.holders() > 1 {
                let _ = payload.spill_to(&file);
            }
            pending.fetch_sub(len, Relaxed);
        }
    });
    *spill.worker.lock().unwrap() = Some(tx);
    Arc::new(spill)
}

fn settle(spill: &Spill) {
    let start = Instant::now();
    while spill.pending() > 0 && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn ring_keeps_full_length_by_spilling_past_the_ram_budget() {
    // 20 s at 60 fps of 10 KB packets = 12 MB; RAM budget 2 MB.
    let spill = test_spill(2 * MIB, 64 * MIB);
    let mut ring = PacketRing::with_spill(20_000_000, Some(Arc::clone(&spill)));
    for i in 0..1200 {
        ring.push(packet(i, 10_000));
        if i % 100 == 0 {
            settle(&spill);
        }
    }
    settle(&spill);
    assert_eq!(ring.span_frames(), 1200, "nothing dropped");
    assert_eq!(ring.dropped_for_ram(), 0);
    assert!(spill.ram() <= 2 * MIB + 10_000, "RAM {} stays near the budget", spill.ram());
    // A full-length save reads every packet back, oldest from disk.
    let window = ring.save_window(20_000_000);
    assert_eq!(window.len(), 1200);
    assert!(!window[0].data.in_ram());
    for (i, p) in window.iter().enumerate() {
        assert_eq!(p.data.read().unwrap().as_ref(), &vec![i as u8; 10_000][..]);
    }
}

#[test]
fn ring_drops_old_seconds_when_spilling_is_impossible() {
    let spill = Arc::new(Spill::new(Budget { total: 0, audio: 0, forced: Some(MIB) }, SpillPlan { want_file: 0 }, quiet()));
    spill.dead.store(true, Relaxed);
    let mut ring = PacketRing::with_spill(60_000_000, Some(Arc::clone(&spill)));
    for i in 0..3600 {
        ring.push(packet(i, 100_000)); // 360 MB asked for
    }
    assert!(ring.dropped_for_ram() > 0);
    let hard = MIB + 64 * MIB;
    assert!(spill.ram() <= hard, "RAM {} held under the hard limit {hard}", spill.ram());
    assert!(ring.first_pts_us().is_some());
    assert!(ring.save_window(60_000_000)[0].keyframe, "still starts on a keyframe");
}

#[test]
fn ring_drops_packets_the_spill_file_overwrote() {
    // A file far smaller than the buffer: the ring gives up the overwritten head.
    let spill = test_spill(MIB / 2, 2 * MIB);
    let mut ring = PacketRing::with_spill(60_000_000, Some(Arc::clone(&spill)));
    for i in 0..600 {
        ring.push(packet(i, 10_000));
        settle(&spill);
    }
    let window = ring.save_window(60_000_000);
    assert!(window.len() < 600);
    assert!(window[0].keyframe);
    assert!(window.iter().all(|p| p.data.read().is_ok()), "every packet left in the ring is readable");
}

/// Prints this PC's fixed drives (disk number, SSD flag, free space). Run with
/// `cargo test -p recall drive_probe -- --ignored --nocapture`.
#[test]
#[ignore]
fn drive_probe_prints_this_pcs_drives() {
    let drives = super::drives::sys::fixed_drives();
    for d in &drives {
        println!(
            "drive {} disk={:?} ssd={:?} free={} GiB system={}",
            d.root,
            d.disk,
            d.ssd,
            d.free / GB,
            d.system
        );
    }
    assert!(!drives.is_empty(), "at least the Windows drive is fixed");
}

#[test]
fn spill_file_names_carry_the_pid() {
    use super::sweep::spill_pid;
    assert_eq!(spill_pid("recall-replay-4242.spill"), Some(4242));
    assert_eq!(spill_pid("recall-replay-x.spill"), None);
    assert_eq!(spill_pid("clip.mp4"), None);
    assert_eq!(spill_pid("recall-replay-4242.spill.mp4"), None);
}

#[test]
fn sweep_removes_only_dead_runs_spill_files() {
    use super::sweep::sweep_dir;
    let dir = std::env::temp_dir().join(format!("recall-sweep-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("recall-replay-111.spill"), vec![0u8; 3000]).unwrap(); // dead run
    std::fs::write(dir.join("recall-replay-222.spill"), vec![0u8; 10]).unwrap(); // running recall
    std::fs::write(dir.join("clip.mp4"), b"keep").unwrap();
    std::fs::write(dir.join("recall-replay-333.spill.bak"), b"keep").unwrap();
    let (files, bytes) = sweep_dir(&dir, &|pid| pid == 222);
    assert_eq!((files, bytes), (1, 3000));
    assert!(!dir.join("recall-replay-111.spill").exists());
    assert!(dir.join("recall-replay-222.spill").exists());
    assert!(dir.join("clip.mp4").exists());
    assert!(dir.join("recall-replay-333.spill.bak").exists());
    assert_eq!(sweep_dir(&dir.join("missing"), &|_| false), (0, 0), "a missing folder is fine");
    let _ = std::fs::remove_dir_all(&dir);
}
