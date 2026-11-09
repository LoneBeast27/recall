//! Writer thread: generations, parts, encoder failure, crash recovery (fake sink).

use super::*;

use std::sync::mpsc::sync_channel;
use test_sink::{pkt, temp_dir, tracks, Call, FakeFactory, Log, MS};

thread_local! {
    /// The last `recording_stopped` this thread's writer sent (`stopped`).
    pub(super) static LAST_STOP: std::cell::RefCell<Option<serde_json::Value>> = const { std::cell::RefCell::new(None) };
}

/// Generations far above the live encoder tests' (the failure slot is global).
const G: u64 = 1_000_000;

fn writer(tag: &str, generation: u64) -> (Writer<FakeFactory>, PathBuf) {
    let dir = temp_dir(tag);
    let recorder = Recorder::new(FakeFactory::default(), tracks());
    (Writer { out_dir: dir.to_string_lossy().into_owned(), json: false, recorder, generation, skew: Duration::ZERO, parts_so_far: Vec::new(), missed: (0.0, 0.0), lock: LockClock::default() }, dir)
}

fn video(pts_ms: u64, keyframe: bool, generation: u64) -> Data {
    Data::Video { pts_us: pts_ms * MS, keyframe, data: pkt(pts_ms as u8), generation }
}

/// The single finished recording in `dir` (and no `.part` left).
fn finished(dir: &Path) -> Vec<String> {
    let names: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|n| !n.ends_with(".part")), "{names:?}");
    names
}

#[test]
fn a_reopened_encoders_first_packet_continues_the_recording_without_the_notice() {
    let (mut w, dir) = writer("reopen", 0);
    let (_tx, rx) = sync_channel::<Data>(4);
    w.start("G", Some((0, Instant::now())));
    w.generation = G + 3; // the encoder generation the recording started on
    w.data(video(0, true, G + 3));
    w.data(video(16, false, G + 3));
    assert!(w.recorder.is_active());
    w.data(video(20, false, G + 2)); // a stale packet of an older encoder: skipped
    assert!(w.recorder.is_active());
    // The EncoderReopened notice was dropped (queue full); the new
    // encoder's first keyframe ends part 1 and starts part 2 (past the part gap).
    w.skew = machine::PART_GAP;
    w.data(video(33, true, G + 4));
    assert_eq!(w.generation, G + 4);
    assert!(w.recorder.is_active(), "the recording goes on in a new file");
    w.data(video(50, false, G + 4));
    w.check_encoder_failed(&rx); // nothing failed
    w.stop(&rx);
    let mut names = finished(&dir);
    names.sort();
    assert_eq!(names.len(), 2, "two finished parts: {names:?}");
    assert!(names[0].ends_with("__rec-2.mp4") && names[1].ends_with("__rec.mp4"), "{names:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_encoder_ends_the_recording_after_its_queued_packets() {
    // A generation no other test's writer uses (the failure is global).
    let generation = u64::MAX - 7;
    let _failures = tap::FAILED_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (mut w, dir) = writer("encfail", generation);
    let (tx, rx) = sync_channel::<Data>(4);
    w.start("G", Some((0, Instant::now())));
    w.generation = generation;
    w.data(video(0, true, generation));
    tx.send(video(16, false, generation)).unwrap();
    w.check_encoder_failed(&rx);
    assert!(w.recorder.is_active(), "no failure reported yet");
    tap::on_encoder_failed_at(generation, "NVENC encode failed: device lost");
    w.check_encoder_failed(&rx);
    assert!(!w.recorder.is_active(), "the recording ended");
    assert!(rx.try_recv().is_err(), "the queued packet was written first");
    assert_eq!(finished(&dir).len(), 1, "the file was finished and kept");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Merge gate 11-07 (P2): a part opened with the newest encoder's settings, which can
/// be newer than its packets (generation 5 open before generation 4's first packet).
#[test]
fn a_part_opens_with_its_own_encoders_settings_not_a_newer_ones() {
    let (mut w, dir) = writer("snapshot", 0);
    let log = Log::default();
    w.recorder = Recorder::new(FakeFactory { log: log.clone(), newest: G + 5, ..Default::default() }, tracks());
    let (_tx, rx) = sync_channel::<Data>(4);
    w.start("G", Some((0, Instant::now())));
    assert_eq!(w.generation, G + 5, "the start reads the newest generation with its settings");
    // As if the recording had started on generation 3 (its settings: 3).
    w.recorder.set_video(Some(G + 3));
    w.generation = G + 3;
    w.data(video(0, true, G + 3));
    w.data(video(16, false, G + 3));
    w.skew = machine::PART_GAP;
    w.data(video(33, true, G + 4)); // generation 4's first packet; 5 is already the newest
    w.data(video(50, false, G + 4));
    w.stop(&rx);
    let params: Vec<u64> = log.calls().into_iter().filter_map(|c| if let Call::Params(g) = c { Some(g) } else { None }).collect();
    assert_eq!(params, vec![G + 3, G + 4], "part 2 has generation 4's settings");
    assert_eq!(finished(&dir).len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The encoder failed, then reopened (encoder/reopen.rs) before the writer polled the
/// failure: the recording ends with the failed encoder, never "continues" silently.
#[test]
fn a_failed_encoder_that_already_reopened_ends_the_recording() {
    let generation = u64::MAX - 20;
    let _failures = tap::FAILED_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (mut w, dir) = writer("failreopen", generation);
    w.start("G", Some((0, Instant::now())));
    w.generation = generation;
    w.data(video(0, true, generation));
    w.data(video(16, false, generation));
    tap::on_encoder_failed_at(generation, "NVENC encode failed: device lost");
    w.skew = machine::PART_GAP;
    w.data(video(33, true, generation + 1)); // the reopened encoder's first packet came first
    assert!(!w.recorder.is_active(), "the failure ends the recording");
    let names = finished(&dir);
    assert_eq!(names.len(), 1, "no part 2: {names:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Merge gate 11-07 #3 (P1): a failure named only the last file, so the toast could not
/// say which files the recording left.
#[test]
fn a_failure_after_a_continued_part_names_every_file() {
    let generation = u64::MAX - 40;
    let _failures = tap::FAILED_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (mut w, dir) = writer("failparts", generation);
    let (_tx, rx) = sync_channel::<Data>(4);
    w.start("G", Some((0, Instant::now())));
    w.generation = generation;
    w.data(video(0, true, generation));
    w.data(video(16, false, generation));
    w.skew = machine::PART_GAP;
    w.data(video(33, true, generation + 1)); // part 2
    w.data(video(50, false, generation + 1));
    assert_eq!(w.parts_so_far.len(), 1, "part 1 is finished");
    tap::on_encoder_failed_at(generation + 1, "NVENC encode failed: device lost");
    let first = w.parts_so_far.clone();
    w.check_encoder_failed(&rx);
    assert!(!w.recorder.is_active() && w.parts_so_far.is_empty());
    let mut names = finished(&dir);
    names.sort();
    assert_eq!(names.len(), 2, "{names:?}");
    let out = serde_json::to_value(failed_outbound("the video encoder failed", Some(&dir.join(&names[0])), &first, (0.0, 0.0))).unwrap();
    let paths: Vec<&str> = out["paths"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
    assert_eq!(paths.len(), 2, "{out}");
    assert!(paths[0].ends_with("__rec.mp4") && paths[1].ends_with("__rec-2.mp4"), "{paths:?}");
    assert_eq!(out["type"], "recording_failed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Merge gate 11-07 (P2): a failure while the next part waits named the finished part as
/// both an earlier part and the kept file, so the toast listed it twice.
#[test]
fn a_failure_between_parts_names_the_finished_part_once() {
    let generation = u64::MAX - 60;
    let (mut w, dir) = writer("failbetween", generation);
    w.start("G", Some((0, Instant::now())));
    w.generation = generation;
    w.data(video(0, true, generation));
    w.data(video(16, false, generation));
    // Reopened inside the part gap: part 1 is finished, part 2 waits.
    w.data(Data::EncoderReopened { generation: generation + 1, at: Instant::now() });
    assert_eq!(w.parts_so_far.len(), 1, "part 1 is finished");
    let first = w.parts_so_far.clone();
    let events = w.recorder.encoder_failed("device lost");
    let [Event::Failed { reason, kept }] = &events[..] else { panic!("{events:?}") };
    assert_eq!(kept.as_ref(), first.first(), "the kept file is the finished part");
    let out = serde_json::to_value(failed_outbound(reason, kept.as_deref(), &first, w.missed)).unwrap();
    let paths: Vec<&str> = out["paths"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
    assert_eq!(paths.len(), 1, "named once: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Merge gate 11-07 #4 (P2): a recording that ended early after a freeze did not say so.
#[test]
fn a_failure_reports_the_seconds_frozen_before_it() {
    let generation = u64::MAX - 80;
    let dir = temp_dir("failfrozen");
    let factory = FakeFactory { causes: vec![(generation + 1, machine::Cause::Same)], ..Default::default() };
    let recorder = Recorder::new(factory, tracks());
    let mut w = Writer { out_dir: dir.to_string_lossy().into_owned(), json: false, recorder, generation, skew: Duration::ZERO, parts_so_far: Vec::new(), missed: (0.0, 0.0), lock: LockClock::default() };
    let t0 = Instant::now();
    w.start("G", Some((0, t0)));
    w.generation = generation;
    w.data(video(0, true, generation));
    w.data(video(1_000, false, generation)); // the last packet before an encode error
    // The same-header encoder is back 4 s later, on a keyframe.
    w.data(Data::EncoderReopened { generation: generation + 1, at: t0 + Duration::from_secs(5) });
    w.data(video(5_000, true, generation + 1));
    w.data(video(5_016, false, generation + 1));
    let events = w.recorder.encoder_failed("device lost");
    let [Event::Failed { reason, kept }] = &events[..] else { panic!("{events:?}") };
    let out = serde_json::to_value(failed_outbound(reason, kept.as_deref(), &[], w.missed)).unwrap();
    let frozen = out["frozen_seconds"].as_f64().unwrap();
    assert!((frozen - 4.0).abs() < 0.01, "1 s -> 5 s held still: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Merge gate 11-07 (P2): the writer starts a same-header recovery's freeze at the old
/// encoder's encode error (`tap::on_encode_error`), not at its last packet.
#[test]
fn the_writer_counts_a_freeze_from_the_encode_error() {
    let generation = u64::MAX - 100;
    let dir = temp_dir("errfrozen");
    let factory = FakeFactory { causes: vec![(generation + 1, machine::Cause::Same)], ..Default::default() };
    let recorder = Recorder::new(factory, tracks());
    let mut w = Writer { out_dir: dir.to_string_lossy().into_owned(), json: false, recorder, generation, skew: Duration::ZERO, parts_so_far: Vec::new(), missed: (0.0, 0.0), lock: LockClock::default() };
    let t0 = Instant::now();
    w.start("G", Some((0, t0)));
    w.generation = generation;
    w.data(video(0, true, generation));
    w.data(video(1_000, false, generation)); // then a static screen: no packets
    tap::on_encode_error_at(generation, t0 + Duration::from_secs(6));
    w.data(Data::EncoderReopened { generation: generation + 1, at: t0 + Duration::from_secs(9) });
    w.data(video(9_000, true, generation + 1));
    w.data(video(9_016, false, generation + 1));
    let (_, frozen) = w.missed;
    assert!((frozen - 3.0).abs() < 0.01, "the error at 6 s to the keyframe at 9 s, not 8 s: {frozen}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recovery_renames_only_long_recording_parts_and_skips_open_files() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = std::env::temp_dir().join(format!("recall-recover-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let crashed = dir.join("G__2026-10-22__14-05-09__rec.mp4.part");
    let live = dir.join("G__2026-10-22__15-00-00__rec.mp4.part");
    let replay = dir.join("G__2026-10-22__14-05-09-275__ab12.mp4.part");
    let second = dir.join("G__2026-10-22__14-05-09__rec-2.mp4.part");
    for p in [&crashed, &live, &replay, &second] {
        std::fs::write(p, b"x").unwrap();
    }
    // A recorder holds `live` open the way Fmp4Sink does (no delete sharing).
    let _held = std::fs::OpenOptions::new().write(true).share_mode(0x1).open(&live).unwrap();
    let results = recover_in(&dir);
    assert_eq!(results.len(), 3, "replay parts are not ours");
    assert!(dir.join("G__2026-10-22__14-05-09__rec.mp4").is_file(), "crashed part recovered");
    assert!(dir.join("G__2026-10-22__14-05-09__rec-2.mp4").is_file(), "a same-second second recording too");
    assert!(!crashed.exists());
    assert!(live.exists(), "a file being written is left alone");
    assert!(replay.exists());
    drop(_held);
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Sort clips into game folders": a recording left in a marked `<out>\<Game>\` is recovered
/// there; an unmarked folder and two levels down are not Recall's.
#[test]
fn recovery_reaches_game_folders_one_level_down() {
    struct Gone(PathBuf);
    impl Drop for Gone {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = std::env::temp_dir().join(format!("recall-recover-games-{}", uuid::Uuid::new_v4().simple()));
    let _gone = Gone(dir.clone());
    let game = dir.join("Rocket League");
    let deeper = game.join("old");
    std::fs::create_dir_all(&deeper).unwrap();
    std::fs::write(game.join(crate::game_folder_rule::MARKER), b"").unwrap();
    let keepers = dir.join("Keepers");
    std::fs::create_dir_all(&keepers).unwrap();
    let top = dir.join("Desktop__2026-10-22__14-05-09__rec.mp4.part");
    let in_game = game.join("Rocket-League__2026-10-22__15-00-00__rec.mp4.part");
    let too_deep = deeper.join("Rocket-League__2026-10-22__16-00-00__rec.mp4.part");
    let unmarked = keepers.join("Rocket-League__2026-10-22__17-00-00__rec.mp4.part");
    for p in [&top, &in_game, &too_deep, &unmarked] {
        std::fs::write(p, b"x").unwrap();
    }
    let results = recover_in(&dir);
    assert_eq!(results.len(), 2);
    assert!(dir.join("Desktop__2026-10-22__14-05-09__rec.mp4").is_file());
    assert!(game.join("Rocket-League__2026-10-22__15-00-00__rec.mp4").is_file(), "recovered in its game folder");
    assert!(too_deep.exists());
    assert!(unmarked.exists(), "a folder Recall did not make is the user's");
}

/// Merge gate 11-07 (P1): an NVENC hang mid-recording sends no packets and no failure;
/// the controller ends the recording with `record_fail` before it restarts the engine.
#[test]
fn record_fail_ends_a_continued_recording_early_and_keeps_every_part() {
    let generation = u64::MAX - 60;
    let (mut w, dir) = writer("recordfail", generation);
    let (tx, rx) = sync_channel::<Data>(4);
    w.start("G", Some((0, Instant::now())));
    w.generation = generation;
    w.data(video(0, true, generation));
    w.data(video(16, false, generation));
    w.skew = machine::PART_GAP;
    w.data(video(33, true, generation + 1)); // part 2
    tx.send(video(50, false, generation + 1)).unwrap();
    assert_eq!(w.parts_so_far.len(), 1);
    w.control(Control::Fail { reason: "the encoder isn't responding".into() }, &rx);
    assert!(!w.recorder.is_active(), "the recording ended");
    assert!(rx.try_recv().is_err(), "the queued packet was written first");
    let mut names = finished(&dir);
    names.sort();
    assert_eq!(names.len(), 2, "both parts kept and finished: {names:?}");
    w.control(Control::Fail { reason: "again".into() }, &rx); // nothing running: ignored
    let _ = std::fs::remove_dir_all(&dir);
}

/// Gate P2-c (d1e057e): several `.part` files recovered at once were one event each, so
/// the app showed a toast per file; one event lists them all.
#[test]
fn several_recovered_parts_are_one_event_that_lists_them() {
    assert!(recovered_outbound(Vec::new()).is_none());
    let out = recovered_outbound(vec!["E:/c/G__rec.mp4".into(), "E:/c/H__rec.mp4".into()]).unwrap();
    let v = serde_json::to_value(out).unwrap();
    assert_eq!(v["type"], "recording_recovered");
    assert_eq!(v["path"], "E:/c/G__rec.mp4", "older readers still get the first file");
    assert_eq!(v["paths"], serde_json::json!(["E:/c/G__rec.mp4", "E:/c/H__rec.mp4"]));
}

/// Audit U8: the time the PC sat locked (Win+L) during a recording is counted on the
/// writer's clock and reset by the stop.
#[test]
fn a_twelve_minute_lock_during_a_recording_is_counted_and_reset_by_the_stop() {
    let (mut w, dir) = writer("locked", G + 50);
    let (_tx, rx) = sync_channel::<Data>(4);
    w.start("G", Some((0, Instant::now())));
    w.generation = G + 50;
    w.data(video(0, true, G + 50));
    w.control(Control::Locked(true), &rx);
    w.skew = Duration::from_secs(720);
    w.control(Control::Locked(false), &rx);
    let locked = w.lock.seconds(w.now());
    assert!((locked - 720.0).abs() < 0.5, "{locked}");
    w.data(video(16, false, G + 50));
    w.stop(&rx);
    assert_eq!(w.lock.seconds(w.now()), 0.0, "the next recording starts from zero");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Audit U8 / gate 20b P2-e: the `recording_stopped` the writer sends at the stop carries the
/// time locked (no hand-built message: the stop's own outbound).
#[test]
fn the_writers_stop_message_carries_the_locked_seconds() {
    let (mut w, dir) = writer("locked-stop", G + 51);
    let (_tx, rx) = sync_channel::<Data>(4);
    w.start("G", Some((0, Instant::now())));
    w.generation = G + 51;
    w.data(video(0, true, G + 51));
    w.control(Control::Locked(true), &rx);
    w.skew = Duration::from_secs(720);
    w.control(Control::Locked(false), &rx);
    w.data(video(16, false, G + 51));
    LAST_STOP.with(|last| last.borrow_mut().take());
    w.stop(&rx);
    let out = LAST_STOP.with(|last| last.borrow_mut().take()).expect("a recording_stopped was sent");
    assert_eq!(out["type"], "recording_stopped");
    assert_eq!(out["continued"], false);
    let locked = out["locked_seconds"].as_f64().unwrap();
    assert!((locked - 720.0).abs() < 0.5, "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
