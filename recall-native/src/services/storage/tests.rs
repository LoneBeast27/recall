//! Pure-logic tests: drive classification, descriptor parsing, identity keys, LRU plans,
//! lane priority, prefetch order, project precedence / flush rule, thumbnail parsing.
use super::drive::{BUS_NVME, classify, parse_descriptor};
use super::lane::{LaneQueue, Priority};
use super::prefetch::{PrefetchClip, Step, plan};
use super::smr::is_drive_managed_smr;
use super::thumbs::{jpeg_size, recorder_thumb_parts};
use super::*;

const SATA: u32 = 11;
const USB: u32 = 7;

#[test]
fn smr_models_match_known_drive_managed_families() {
    // This machine: V: and B: are ST4000DM004 (SMR); R: is ST28000DM000 (CMR).
    assert!(is_drive_managed_smr("ST4000DM004-2CV104"));
    assert!(!is_drive_managed_smr("ST28000DM000-3Y9103"));
    assert!(is_drive_managed_smr("ST8000DM004-2CX188"));
    assert!(is_drive_managed_smr("WDC WD40EFAX-68JH4N0"));
    assert!(!is_drive_managed_smr("WDC WD40EFRX-68N32N0"), "WD Red Plus is CMR");
    assert!(!is_drive_managed_smr("WDC WD40EFZX-68AWUN0"));
    assert!(is_drive_managed_smr("WDC WD20EZAZ-00GGJB0"));
    assert!(!is_drive_managed_smr("WDC WD80EZAZ-11TDBA0"), "8 TB EZAZ is helium CMR");
    assert!(is_drive_managed_smr("ST2000LM015-2E8174"));
    assert!(is_drive_managed_smr("st5000lm000-2an170"), "case-insensitive");
    assert!(!is_drive_managed_smr("ST1000LM049-2GH172"), "BarraCuda Pro 2.5 is CMR");
    assert!(is_drive_managed_smr("TOSHIBA HDWD240"));
    assert!(!is_drive_managed_smr("Samsung SSD 980 PRO 1TB"));
    assert!(!is_drive_managed_smr("CT2000P3PSSD8"));
    assert!(!is_drive_managed_smr("STLM"));
    assert!(!is_drive_managed_smr(""));
}

#[test]
fn classification_prefers_smr_model_then_seek_penalty() {
    assert_eq!(classify(Some(true), "ST4000DM004-2CV104", SATA), DriveKind::SmrHdd);
    assert_eq!(classify(Some(true), "ST28000DM000-3Y9103", SATA), DriveKind::Hdd);
    assert_eq!(classify(Some(false), "Samsung SSD 980 PRO 1TB", BUS_NVME), DriveKind::Ssd);
    assert_eq!(classify(None, "CT2000P3PSSD8", BUS_NVME), DriveKind::Ssd);
    assert_eq!(classify(None, "Generic USB enclosure", USB), DriveKind::Hdd, "unknown counts as HDD");
    assert_eq!(classify(None, "ST4000DM004-2CV104", USB), DriveKind::SmrHdd, "SMR inside an enclosure");
    assert_eq!(DriveKind::SmrHdd.label(), "SMR HDD");
    assert!(DriveKind::SmrHdd.is_hdd() && DriveKind::Hdd.is_hdd() && !DriveKind::Ssd.is_hdd());
}

/// STORAGE_DEVICE_DESCRIPTOR with strings appended after the 36-byte header.
fn descriptor(vendor: &str, product: &str, bus: u32) -> Vec<u8> {
    let mut buffer = vec![0u8; 40];
    let put = |buffer: &mut Vec<u8>, text: &str| -> u32 {
        if text.is_empty() {
            return 0;
        }
        let offset = buffer.len() as u32;
        buffer.extend_from_slice(text.as_bytes());
        buffer.push(0);
        offset
    };
    let vendor_offset = put(&mut buffer, vendor);
    let product_offset = put(&mut buffer, product);
    let size = buffer.len() as u32;
    buffer[4..8].copy_from_slice(&size.to_le_bytes());
    buffer[12..16].copy_from_slice(&vendor_offset.to_le_bytes());
    buffer[16..20].copy_from_slice(&product_offset.to_le_bytes());
    buffer[28..32].copy_from_slice(&bus.to_le_bytes());
    buffer
}

#[test]
fn descriptor_strings_and_bus_are_parsed() {
    let ata = parse_descriptor(&descriptor("", "ST4000DM004-2CV104", SATA)).unwrap();
    assert_eq!((ata.model(), ata.bus), ("ST4000DM004-2CV104".to_string(), SATA));
    let split = parse_descriptor(&descriptor("WDC", "WD40EFAX-68JH4N0", SATA)).unwrap();
    assert_eq!(split.model(), "WDC WD40EFAX-68JH4N0");
    assert_eq!(classify(Some(true), &split.model(), split.bus), DriveKind::SmrHdd);
    let nvme = parse_descriptor(&descriptor("Samsung", "Samsung SSD 980 PRO 1TB", BUS_NVME)).unwrap();
    assert_eq!(nvme.model(), "Samsung SSD 980 PRO 1TB", "vendor already in the product is not repeated");
    assert!(parse_descriptor(&[0u8; 6]).is_none(), "truncated header");
    let mut bad_offset = descriptor("", "X", SATA);
    bad_offset[16..20].copy_from_slice(&999u32.to_le_bytes());
    assert_eq!(parse_descriptor(&bad_offset).unwrap().product, "", "offset past the data is ignored");
}

#[test]
fn identity_is_case_insensitive_and_revision_sensitive() {
    let base = identity("VALORANT__2026-10-17.mp4", 6_000_000, 1_792_256_671);
    assert_eq!(base.len(), 16);
    assert!(base.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(base, identity("valorant__2026-10-17.MP4", 6_000_000, 1_792_256_671));
    assert_ne!(base, identity("VALORANT__2026-10-17.mp4", 6_000_001, 1_792_256_671));
    assert_ne!(base, identity("VALORANT__2026-10-17.mp4", 6_000_000, 1_792_256_672));
    assert_ne!(base, identity("other.mp4", 6_000_000, 1_792_256_671));
    assert_ne!(identity("a", 1, 0), identity("a\u{1}", 0, 0), "name and size do not run together");
}

#[test]
fn lane_queue_runs_most_urgent_then_newest() {
    let mut queue = LaneQueue::new();
    queue.push(Priority::Visible, "v1");
    queue.push(Priority::Background, "b1");
    queue.push(Priority::Interactive, "i1");
    queue.push(Priority::Visible, "v2");
    queue.push(Priority::Interactive, "i2");
    assert_eq!(queue.len(), 5);
    let order: Vec<_> = std::iter::from_fn(|| queue.pop().map(|(_, item)| item)).collect();
    assert_eq!(order, ["i2", "i1", "v2", "v1", "b1"]);
}

pub(super) fn clip(name: &str, has_thumb: bool, has_duration: bool) -> PrefetchClip {
    PrefetchClip {
        path: format!("V:\\clips\\{name}"),
        name: name.into(),
        bytes: 100 << 20,
        mtime_secs: 1,
        has_thumb,
        duration: has_duration.then_some(60.0),
        game_key: "game".into(),
        favourite: false,
        edited: false,
    }
}

#[test]
fn planner_fills_thumbs_first_then_climbs_the_cascade_by_rank() {
    use super::prefetch::{EDITOR_TOP, VIDEO_TOP};
    let clips: Vec<PrefetchClip> = (0..30).map(|i| clip(&format!("c{i}.mp4"), i % 2 == 0, i % 3 == 0)).collect();
    // Rank order deliberately differs from list order.
    let ranked: Vec<usize> = (0..30).rev().collect();
    let order = plan(&clips, &ranked, &|_, _| false);
    let first_heavy = order.iter().position(|(s, _)| !matches!(s, Step::Thumb | Step::Duration)).unwrap();
    assert!(order[..first_heavy].iter().all(|(s, _)| matches!(s, Step::Thumb | Step::Duration)), "thumbs + durations for every clip first");
    assert_eq!(order[..first_heavy].iter().filter(|(s, _)| *s == Step::Thumb).count(), 15);
    assert_eq!(order[..first_heavy].iter().filter(|(s, _)| *s == Step::Duration).count(), 20);
    assert_eq!(order[0].1, 29, "best-ranked clip first");
    assert_eq!(&order[first_heavy..first_heavy + 3], &[(Step::Scrub, 29), (Step::Editor, 29), (Step::Video, 29)], "cheapest tier first per clip");
    let editors = order.iter().filter(|(s, _)| *s == Step::Editor).count();
    let videos: Vec<usize> = order.iter().filter(|(s, _)| *s == Step::Video).map(|(_, i)| *i).collect();
    assert_eq!(editors, EDITOR_TOP);
    assert_eq!(videos, vec![29, 28, 27][..VIDEO_TOP].to_vec(), "video copies only for the top predictions");
    assert_eq!(order.iter().filter(|(s, _)| *s == Step::Scrub).count(), 30);
    let none = plan(&clips, &ranked, &|step, _| step != Step::Duration);
    assert!(none.iter().all(|(s, _)| *s == Step::Duration), "cached work is not planned");
}

#[test]
fn newer_project_wins_with_sidecar_on_ties() {
    use ProjectSource::{Sidecar, WorkingCopy};
    assert_eq!(project_order(Some(200), Some(100)), vec![WorkingCopy, Sidecar]);
    assert_eq!(project_order(Some(100), Some(200)), vec![Sidecar, WorkingCopy]);
    assert_eq!(project_order(Some(100), Some(100)), vec![Sidecar, WorkingCopy]);
    assert_eq!(project_order(Some(1), None), vec![WorkingCopy]);
    assert_eq!(project_order(None, Some(1)), vec![Sidecar]);
    assert!(project_order(None, None).is_empty());
}

#[test]
fn sidecar_is_written_only_for_changes_and_real_edits() {
    assert!(!should_flush(Some(b"same"), b"same", true, false), "identical bytes: no SMR write");
    assert!(should_flush(Some(b"old"), b"new", false, true), "changed sidecar is rewritten");
    assert!(!should_flush(None, b"x", false, true), "opened, never edited: no sidecar");
    assert!(should_flush(None, b"x", true, true), "autosaved edits get a sidecar");
    assert!(should_flush(None, b"x", false, false), "edits not yet autosaved still flush");
}

fn tiny_jpeg(width: u16, height: u16, marker: u8) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    bytes.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    bytes.extend_from_slice(&[0xFF, 0xFF, marker, 0x00, 0x11, 0x08]);
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&[0x03, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    bytes.extend(std::iter::repeat_n(0u8, 160));
    bytes.extend_from_slice(&[0xFF, 0xD9]);
    bytes
}

#[test]
fn jpeg_dimensions_and_recorder_thumb_names() {
    assert_eq!(jpeg_size(&tiny_jpeg(640, 360, 0xC0)), Some((640, 360)));
    assert_eq!(jpeg_size(&tiny_jpeg(1920, 1080, 0xC2)), Some((1920, 1080)), "progressive");
    assert_eq!(jpeg_size(b"\x89PNG\r\n\x1a\n"), None);
    assert_eq!(jpeg_size(&[0xFF, 0xD8, 0xFF]), None);
    assert_eq!(
        recorder_thumb_parts("VALORANT__2026-10-17__22-34-31-275__3d26-1792256671-thumb.jpg"),
        Some(("VALORANT__2026-10-17__22-34-31-275__3d26", 1_792_256_671))
    );
    assert_eq!(recorder_thumb_parts("clip-12-thumb.jpg"), None, "epoch too short");
    assert_eq!(recorder_thumb_parts("clip-1792256671.jpg"), None);
}

#[test]
fn volume_roots_come_from_drive_letters_without_system_calls() {
    use super::drive::volume_root;
    assert_eq!(volume_root(Path::new(r"V:\ALL RECORDINGS\a.mp4")).as_deref(), Some(r"V:\"));
    assert_eq!(volume_root(Path::new(r"\\?\v:\ALL RECORDINGS\a.mp4")).as_deref(), Some(r"V:\"));
    assert_eq!(plain_path(Path::new(r"\\?\UNC\nas\share\a.mp4")), PathBuf::from(r"\\nas\share\a.mp4"));
}
