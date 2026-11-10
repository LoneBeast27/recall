//! design-detect-release P6 through the real saver: the drain gives up a stalled save, the
//! engine answers every save in the saver itself, and the saver's own abort answers are not
//! sent after it (one answer per save, never none).

use super::super::saver_stop::{stop_saver_on, STALLED_REASON};
use super::*;
use crate::awake_clock::FakeClock;

#[test]
fn a_given_up_save_gets_exactly_one_answer_the_engines_own() {
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
    let outcome = stop_saver_on(&done, &abort, &current, Duration::from_millis(400), Some(&progress), &clock, Duration::from_millis(1));
    assert_eq!(outcome, Shutdown::Aborted, "the mux took the abort");
    let engine = crate::protocol::test_emits::of("save_failed");
    assert_eq!(engine.len(), 2, "both saves answered by the give-up: {engine:?}");
    assert!(engine.iter().all(|l| l.contains(STALLED_REASON)));
    let saver: Vec<String> = sink.lock().unwrap().iter().filter(|l| l.contains(r#""type":"save_failed""#)).cloned().collect();
    assert!(saver.is_empty(), "no second answer from the saver: {saver:?}");
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
