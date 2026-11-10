//! design-detect-release P6 through the real saver, and never lose a clip: the drain lets go
//! of a save (stall, 30 min budget, quit) and the saver keeps what was written, one answer per
//! save, never none.

use super::super::mux::tests::{window_job, x264_window};
use super::super::mux::ABORT_AFTER;
use super::super::part_recover::{recover_replay_parts_older, Found};
use super::super::saver_stop::{stop_saver_on, BUDGET_REASON, KEPT_FOR_RECOVERY, STALLED_REASON};
use super::*;
use crate::awake_clock::FakeClock;

#[test]
fn a_given_up_save_gets_exactly_one_answer() {
    crate::protocol::test_emits::install();
    let dir = std::env::temp_dir().join(format!("recall-giveup-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let _cleanup = RemoveOnDrop(dir.clone());
    let (events, sink) = Events::capture(true);
    let (started_tx, started_rx) = channel::<()>();
    let mux: MuxFn = Box::new(move |_part, _job, abort, _timing| {
        started_tx.send(()).unwrap();
        while !abort.load(Relaxed) {
            std::thread::sleep(Duration::from_millis(5)); // a wedged write that takes the abort
        }
        Err(ABORTED.into())
    });
    let (queue, control) = spawn(events, mux, Box::new(|_, _, _, _| {}));
    let job = |name: &str| job(&dir.join(name));
    assert!(queue.submit(job("a.mp4")));
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(queue.submit(job("b.mp4")));
    drop(queue);
    let SaverControl { done, abort, current, mover: _, progress } = control;
    let clock = FakeClock::stepping(Duration::from_secs(1), Duration::ZERO);
    let outcome = stop_saver_on(&done, &abort, &current, Duration::from_millis(400), &progress, true, &clock, Duration::from_millis(1));
    assert_eq!(outcome, Shutdown::Aborted, "the mux took the abort");
    assert!(crate::protocol::test_emits::of("save_failed").is_empty(), "the saver stopped: it answers itself");
    assert_eq!(answers(&sink).len(), 2, "one answer per save: {:?}", answers(&sink));
}

fn answers(sink: &Mutex<Vec<String>>) -> Vec<String> {
    sink.lock().unwrap().iter().filter(|l| l.contains(r#""type":"save"#) && !l.contains("save_progress")).cloned().collect()
}

#[test]
fn a_save_still_writing_past_the_30_min_budget_is_kept_with_one_answer() {
    // (a) The save keeps making progress; at 30 min awake the drain lets go. The abort lands
    // mid-mux (a real libx264 mux, cut after 5 packets): the trailer is written for them and
    // the clip is kept, answered `saved` with the budget as its `partial` reason.
    crate::protocol::test_emits::install();
    let Some((window, params)) = x264_window() else { return eprintln!("libx264 missing; skipped") };
    let dir = std::env::temp_dir().join(format!("recall-giveup-budget-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let _cleanup = RemoveOnDrop(dir.clone());
    let (events, sink) = Events::capture(true);
    let mux: MuxFn = Box::new(move |part, job, abort, timing| {
        while !abort.load(Relaxed) {
            super::super::save_progress::step(); // still moving: never a stall
            std::thread::sleep(Duration::from_millis(1));
        }
        ABORT_AFTER.with(|a| a.set(Some(5)));
        let out = super::super::mux::produce(part, job, &AtomicBool::new(false), timing);
        ABORT_AFTER.with(|a| a.set(None));
        out
    });
    let (queue, control) = spawn(events, mux, Box::new(|_, _, _, _| {}));
    let clip = dir.join("G__long.mp4");
    let secs = window_secs(&window);
    assert!(queue.submit(window_job(&clip.to_string_lossy(), window.clone(), params, secs)));
    drop(queue);
    let SaverControl { done, abort, current, mover: _, progress } = control;
    let clock = FakeClock::stepping(Duration::from_secs(10), Duration::ZERO);
    let outcome = stop_saver_on(&done, &abort, &current, Duration::from_secs(2), &progress, true, &clock, Duration::from_millis(1));
    assert_eq!(outcome, Shutdown::Aborted, "the saver stopped within the reserve");
    assert!(clock.reads() >= 180, "held the full 30 min awake ({} reads of 10 s)", clock.reads());
    let answered = answers(&sink);
    assert_eq!(answered.len(), 1, "exactly one answer: {answered:?}");
    let saved: serde_json::Value = serde_json::from_str(&answered[0]).unwrap();
    assert_eq!(saved["type"], "saved");
    assert_eq!(saved["partial"], BUDGET_REASON, "never reported as a full save");
    assert!(crate::protocol::test_emits::of("save_failed").is_empty());
    assert!(clip.exists() && !part_path(&clip.to_string_lossy()).exists(), "kept as a clip");
    let frames = saved["frames"].as_u64().unwrap();
    assert!(frames > 0 && frames < window.len() as u64, "{frames} of {}", window.len());
}

#[test]
fn a_stalled_save_leaves_its_part_and_the_next_start_keeps_it() {
    // (b) The mux wedges (no progress for 120 s awake) and ignores the abort: the engine
    // answers once and nothing deletes the .part; the next start's recovery keeps it (a cut-off
    // mux has no index to play by a rename) or adopts it (its trailer went out).
    crate::protocol::test_emits::install();
    let dir = std::env::temp_dir().join(format!("recall-giveup-stall-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let _cleanup = RemoveOnDrop(dir.clone());
    let Some(whole) = super::super::part_recover::tests::finished_clip(&dir) else { return eprintln!("libx264 missing; skipped") };
    let cut = super::super::part_recover::tests::cut_before_moov(&whole);
    let (events, sink) = Events::capture(true);
    let (release_tx, release_rx) = channel::<()>();
    let (started_tx, started_rx) = channel::<()>();
    let bytes = cut.clone();
    let mux: MuxFn = Box::new(move |part, _job, _abort, timing| {
        std::fs::write(part, &bytes).unwrap();
        timing.part_packets = 7;
        started_tx.send(()).unwrap();
        let _ = release_rx.recv(); // a hung drive: the abort is never seen
        Err("drive hung".into())
    });
    let (queue, control) = spawn(events, mux, Box::new(|_, _, _, _| {}));
    let clip = dir.join("G__2026-11-10__11-00-00-000__st.mp4");
    assert!(queue.submit(job(&clip)));
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(queue);
    let SaverControl { done, abort, current, mover: _, progress } = control;
    let clock = FakeClock::stepping(Duration::from_secs(1), Duration::ZERO);
    let outcome = stop_saver_on(&done, &abort, &current, Duration::from_millis(400), &progress, true, &clock, Duration::from_millis(1));
    assert_eq!(outcome, Shutdown::TimedOut);
    let engine = crate::protocol::test_emits::of("save_failed");
    assert_eq!(engine.len(), 1, "{engine:?}");
    assert!(engine[0].contains(STALLED_REASON) && engine[0].contains(KEPT_FOR_RECOVERY), "{engine:?}");
    let part = part_path(&clip.to_string_lossy());
    assert_eq!(std::fs::read(&part).unwrap(), cut, "the .part survives the give-up");
    let logs = crate::protocol::test_emits::of("encoder_log");
    assert!(logs.iter().any(|l| l.contains("kept for recovery")), "{logs:?}");

    // The next start (the process is gone; nothing holds the file): the saver listed the part
    // (`in_progress`), so it is recovered from that list; an older build's unlisted part is
    // found by the one-time scan.
    assert!(super::super::in_progress::get(&part).is_some(), "listed for the next start's recovery");
    let finished = dir.join("G__2026-11-10__11-01-00-000__tr.mp4.part");
    std::fs::write(&finished, &whole).unwrap();
    let found = recover_replay_parts_older(&dir, Duration::ZERO);
    let adopted = dir.join("G__2026-11-10__11-01-00-000__tr.mp4");
    assert_eq!(found, vec![Found::Adopted { part: finished, clip: adopted.clone() }], "the listed part is left to its list");
    assert!(part.exists() && adopted.exists());
    let _ = super::super::in_progress::remove(&part);
    drop(release_tx);
    assert!(answers(&sink).is_empty(), "the saver adds no second answer");
}

/// The temp folder goes even when an assert fails.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A job whose video the injected mux never reads (as `saver_tests::job`).
fn job(path: &Path) -> SaveJob {
    let spec = StillSpec { fps: 60, bitrate: 1, codec: crate::encoder::settings::Codec::H264, quality: crate::encoder::settings::Quality::High };
    SaveJob {
        path: path.to_string_lossy().into_owned(),
        seconds: 2,
        requested: Instant::now(),
        secs: 1.5,
        video_end_us: None,
        video: VideoSrc::Still { frame: (2, 2, Arc::new(vec![0; 16])), spec },
        backend: "test",
        game: None,
        audio: Vec::new(),
        thumb: None,
        timing: Timing::default(),
    }
}
