//! Saver queue semantics with injected mux/gallery stages (no FFmpeg, no GPU).

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

/// `%TEMP%\recall-saver-<tag>-<uuid>`, removed when the guard drops, also on a panic.
fn temp_dir(tag: &str) -> crate::encoder::test_scratch::Scratch {
    crate::encoder::test_scratch::scratch("recall-saver", tag)
}

/// A job whose video the injected mux never reads.
fn job(path: &Path) -> SaveJob {
    SaveJob {
        path: path.to_string_lossy().into_owned(),
        game: None,
        seconds: 2,
        requested: Instant::now(),
        secs: 1.5,
        video_end_us: None,
        video: VideoSrc::Still {
            frame: (2, 2, Arc::new(vec![0; 16])),
            spec: StillSpec {
                fps: 60,
                bitrate: 1,
                codec: crate::encoder::settings::Codec::H264,
                quality: crate::encoder::settings::Quality::High,
            },
        },
        backend: "test",
        audio: Vec::new(),
        thumb: None,
        timing: Timing::default(),
    }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn no_gallery() -> GalleryFn {
    Box::new(|_, _, _, _| {})
}

fn lines(sink: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    sink.lock().unwrap().clone()
}

fn of_type(lines: &[String], ty: &str) -> Vec<String> {
    let tag = format!("{{\"type\":\"{ty}\"");
    lines.iter().filter(|l| l.starts_with(&tag)).cloned().collect()
}

/// Event kinds in order, with timing logs told apart from other logs.
fn kinds(lines: &[String]) -> Vec<&'static str> {
    lines
        .iter()
        .map(|l| {
            if l.starts_with(r#"{"type":"saved""#) {
                "saved"
            } else if l.starts_with(r#"{"type":"save_failed""#) {
                "save_failed"
            } else if l.contains("save timing (") {
                "timing"
            } else {
                "log"
            }
        })
        .collect()
}

fn report<'a>(path: &'a str, staged_path: Option<&'a str>, secs: f32, frames: u64, bytes: u64, ms: u64) -> SavedReport<'a> {
    SavedReport { path, staged_path, seconds: secs, frames, bytes, elapsed_ms: ms, video_end_us: None, partial: None }
}

#[test]
fn saved_and_failed_json_match_todays_wire_shape() {
    let (events, sink) = Events::capture(true);
    events.saved(&report(r"E:\Clips\Game__x.mp4", None, 119.146, 7124, 363_691_446, 6934));
    events.saved(&report(r"E:\c\G.mp4", Some(r"F:\s\G.mp4"), 120.0, 7200, 400, 812));
    events.save_failed(QUEUE_FULL);
    events.clip_moved(r"E:\c\G.mp4", 9549, None);
    events.clip_move_failed(r"E:\c\G.mp4", r"F:\s\G.mp4", "disk full");
    events.log("hello");
    assert_eq!(
        lines(&sink),
        vec![
            r#"{"type":"saved","path":"E:\\Clips\\Game__x.mp4","seconds":119.146,"frames":7124,"bytes":363691446,"elapsed_ms":6934,"staged":false}"#,
            r#"{"type":"saved","path":"E:\\c\\G.mp4","seconds":120.0,"frames":7200,"bytes":400,"elapsed_ms":812,"staged":true,"staged_path":"F:\\s\\G.mp4"}"#,
            r#"{"type":"save_failed","reason":"previous save still writing"}"#,
            r#"{"type":"clip_moved","path":"E:\\c\\G.mp4","elapsed_ms":9549}"#,
            r#"{"type":"clip_move_failed","path":"E:\\c\\G.mp4","staged_path":"F:\\s\\G.mp4","reason":"disk full"}"#,
            r#"{"type":"encoder_log","message":"hello"}"#,
        ]
    );
    let (human, sink) = Events::capture(false);
    human.saved(&report("a.mp4", None, 1.5, 3, 10, 5));
    human.saved(&report("b.mp4", Some(r"F:\s\b.mp4"), 1.5, 3, 10, 5));
    human.save_failed("boom");
    human.log("note");
    assert_eq!(
        lines(&sink),
        vec![
            "[recall] saved a.mp4 (1.5s, 3 frames, 10 bytes, 5ms)",
            r"[recall] saved b.mp4 (1.5s, 3 frames, 10 bytes, 5ms) [staged at F:\s\b.mp4]",
            "[recall] save FAILED: boom",
            "[recall] note",
        ]
    );
}

#[test]
fn queue_is_fifo_one_at_a_time_and_rejects_when_full() {
    let dir = temp_dir("fifo");
    let (events, sink) = Events::capture(true);
    let (started_tx, started_rx) = channel::<String>();
    let (gate_tx, gate_rx) = channel::<()>();
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let (busy, peak) = (in_flight.clone(), max_in_flight.clone());
    let mux: MuxFn = Box::new(move |part, job, _abort, _timing| {
        let now = busy.fetch_add(1, SeqCst) + 1;
        peak.fetch_max(now, SeqCst);
        started_tx.send(job.path.clone()).unwrap();
        gate_rx.recv().unwrap();
        std::fs::write(part, job.path.as_bytes()).unwrap();
        busy.fetch_sub(1, SeqCst);
        Ok(Muxed { frames: 3, secs: 1.5, partial: None })
    });
    let galleries = Arc::new(Mutex::new(Vec::new()));
    let seen = galleries.clone();
    let gallery: GalleryFn = Box::new(move |job, secs, bytes, _| {
        seen.lock().unwrap().push((job.path.clone(), secs, bytes));
    });
    let (queue, control) = spawn(events, mux, gallery);
    let paths: Vec<PathBuf> = ["a", "b", "c", "d"]
        .iter()
        .map(|n| dir.join(format!("{n}.mp4")))
        .collect();
    let wait = Duration::from_secs(5);

    assert!(queue.submit(job(&paths[0])));
    assert_eq!(started_rx.recv_timeout(wait).unwrap(), text(&paths[0]));
    // A is being written; B and C fill the queue; D must fail immediately.
    assert!(queue.submit(job(&paths[1])));
    assert!(queue.submit(job(&paths[2])));
    assert!(!queue.submit(job(&paths[3])), "a third waiting save must be rejected");
    for next in &paths[1..3] {
        gate_tx.send(()).unwrap();
        assert_eq!(started_rx.recv_timeout(wait).unwrap(), text(next));
    }
    gate_tx.send(()).unwrap();
    drop(queue);
    assert_eq!(control.shutdown(Duration::from_secs(3)), Shutdown::Drained);

    assert_eq!(max_in_flight.load(SeqCst), 1, "saves overlapped");
    for p in &paths[..3] {
        assert_eq!(std::fs::read(p).unwrap(), text(p).as_bytes());
        assert!(!part_path(&text(p)).exists());
    }
    assert!(!paths[3].exists());
    let all = lines(&sink);
    assert_eq!(
        kinds(&all),
        vec!["save_failed", "saved", "timing", "saved", "timing", "saved", "timing"]
    );
    assert_eq!(
        of_type(&all, "save_failed"),
        vec![r#"{"type":"save_failed","reason":"previous save still writing","rejected":true}"#],
        "the newest save, failed before it was queued: marked so the island ends its ring, not the oldest's"
    );
    let saved = of_type(&all, "saved");
    for (line, p) in saved.iter().zip(&paths) {
        let path_json = serde_json::to_string(&text(p)).unwrap();
        let bytes = text(p).len();
        let prefix = format!(
            r#"{{"type":"saved","path":{path_json},"seconds":1.5,"frames":3,"bytes":{bytes},"elapsed_ms":"#
        );
        assert!(line.starts_with(&prefix), "{line}");
        let elapsed = line[prefix.len()..].strip_suffix(r#","staged":false}"#).expect(line);
        assert!(elapsed.parse::<u64>().is_ok(), "{line}");
    }
    let galleries = galleries.lock().unwrap().clone();
    let expected: Vec<(String, f32, u64)> = paths[..3]
        .iter()
        .map(|p| (text(p), 1.5, text(p).len() as u64))
        .collect();
    assert_eq!(galleries, expected);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_mux_leaves_no_clip_and_a_panic_keeps_its_part() {
    let dir = temp_dir("fail");
    let (events, sink) = Events::capture(true);
    let mut calls = 0;
    let mux: MuxFn = Box::new(move |part, _job, _abort, _timing| {
        calls += 1;
        std::fs::write(part, b"half a clip").unwrap();
        match calls {
            1 => Err("mux exploded".into()),
            2 => panic!("injected mux panic"),
            _ => Ok(Muxed { frames: 1, secs: 1.5, partial: None }),
        }
    });
    let (queue, control) = spawn(events, mux, no_gallery());
    let paths: Vec<PathBuf> = ["x", "y", "z"]
        .iter()
        .map(|n| dir.join(format!("{n}.mp4")))
        .collect();
    for p in &paths {
        assert!(queue.submit(job(p)));
        // One at a time so the bounded queue never rejects in this test.
        let deadline = Instant::now() + Duration::from_secs(5);
        while lines(&sink).iter().filter(|l| l.contains(r#""type":"save"#)).count()
            < paths.iter().position(|q| q == p).unwrap() + 1
        {
            assert!(Instant::now() < deadline, "saver stalled");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    drop(queue);
    assert_eq!(control.shutdown(Duration::from_secs(3)), Shutdown::Drained);
    for p in &paths[..2] {
        assert!(!p.exists(), "{p:?} left behind");
    }
    // No packet went in (the fake reports none): the failed save's .part is deleted, logged.
    assert!(!part_path(&text(&paths[0])).exists(), "zero-packet .part left behind");
    assert!(lines(&sink).iter().any(|l| l.contains("no packet was written")), "the delete is logged");
    // A panic never deletes (never lose a clip): the next start's recovery sees the .part.
    assert!(part_path(&text(&paths[1])).exists(), "a panicking save's .part is kept");
    assert!(paths[2].exists(), "the saver must survive a panicking job");
    assert_eq!(
        of_type(&lines(&sink), "save_failed"),
        vec![
            r#"{"type":"save_failed","reason":"mux exploded"}"#,
            r#"{"type":"save_failed","reason":"internal error while writing the clip"}"#,
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shutdown_aborts_inflight_save_and_fails_queued_ones() {
    let dir = temp_dir("abort");
    let (events, sink) = Events::capture(true);
    let (started_tx, started_rx) = channel::<()>();
    let mux: MuxFn = Box::new(move |part, _job, abort, _timing| {
        std::fs::write(part, b"partial").unwrap();
        started_tx.send(()).unwrap();
        while !abort.load(Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(ABORTED.into())
    });
    let (queue, control) = spawn(events, mux, no_gallery());
    let (a, b) = (dir.join("a.mp4"), dir.join("b.mp4"));
    assert!(queue.submit(job(&a)));
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(queue.submit(job(&b)));
    drop(queue);

    let started = Instant::now();
    assert_eq!(control.shutdown(Duration::from_millis(600)), Shutdown::Aborted);
    assert!(started.elapsed() < Duration::from_millis(900), "{:?}", started.elapsed());
    assert!(!a.exists() && !part_path(&text(&a)).exists() && !b.exists());
    assert_eq!(
        of_type(&lines(&sink), "save_failed"),
        vec![
            r#"{"type":"save_failed","reason":"save aborted: engine quitting"}"#,
            r#"{"type":"save_failed","reason":"engine quit before the save started"}"#,
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn idle_shutdown_is_immediate() {
    let (events, _sink) = Events::capture(true);
    let mux: MuxFn = Box::new(|_, _, _, _| unreachable!("no job submitted"));
    let (queue, control) = spawn(events, mux, no_gallery());
    drop(queue);
    let started = Instant::now();
    assert_eq!(control.shutdown(Duration::from_secs(3)), Shutdown::Drained);
    assert!(started.elapsed() < Duration::from_millis(500));
}

// ---- SSD staging (temp dirs; the volume check is injected) ----

const CLIP: &str = "G__2026-10-23__10-00-00-000__ab12.mp4";

fn mover_cfg(same_volume: staging::VolumeCheck) -> mover::Config {
    mover::Config {
        chunk: 4096,
        sync_every: 16_384,
        retry_after: Duration::from_millis(50),
        same_volume,
        background: false,
        stamp: mover::stamp_mtime,
        identity: mover::same_identity,
        stat: mover::stat_dest,
        flush_dest: mover::flush_dest,
    }
}

fn setup(stage: Option<&Path>, recover: &Path, out: &Path, same: staging::VolumeCheck) -> staging::Setup {
    staging::Setup {
        dir: stage.map(Path::to_path_buf),
        recover: Some(recover.to_path_buf()),
        out_dir: out.to_path_buf(),
        mover: mover_cfg(same),
        refused: Vec::new(),
    }
}

fn wait_for(sink: &Arc<Mutex<Vec<String>>>, ty: &str, n: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let found = of_type(&lines(sink), ty);
        if found.len() >= n {
            return found;
        }
        assert!(Instant::now() < deadline, "no {ty}: {:?}", lines(sink));
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn json(line: &str) -> serde_json::Value {
    serde_json::from_str(line).unwrap()
}

fn fixed_mux(bytes: usize) -> MuxFn {
    Box::new(move |part, _job, _abort, _timing| {
        std::fs::write(part, vec![7u8; bytes]).unwrap();
        Ok(Muxed { frames: 3, secs: 1.5, partial: None })
    })
}

#[test]
fn staged_save_reports_the_final_path_then_the_mover_puts_it_there() {
    let (stage, out) = (temp_dir("stage"), temp_dir("out"));
    let (events, sink) = Events::capture(true);
    let galleries = Arc::new(Mutex::new(Vec::new()));
    let seen = galleries.clone();
    let gallery: GalleryFn = Box::new(move |job, _, bytes, _| seen.lock().unwrap().push((job.path.clone(), bytes)));
    let cfg = setup(Some(&stage), &stage, &out, |_, _| false);
    let (queue, control) = spawn_staged(events, fixed_mux(50_000), gallery, Some(cfg));
    let final_path = out.join(CLIP);
    assert!(queue.submit(job(&final_path)));

    let moved = wait_for(&sink, "clip_moved", 1);
    let saved = json(&of_type(&lines(&sink), "saved")[0]);
    assert_eq!(saved["path"], text(&final_path), "saved names the FINAL path");
    assert_eq!(saved["staged"], true);
    assert_eq!(saved["staged_path"], text(&stage.join(CLIP)));
    assert_eq!(saved["bytes"], 50_000);
    let moved = json(&moved[0]);
    assert_eq!(moved["path"], text(&final_path));
    assert!(moved["elapsed_ms"].is_u64());
    let all = lines(&sink);
    let at = |ty: &str| all.iter().position(|l| l.starts_with(&format!("{{\"type\":\"{ty}\""))).unwrap();
    assert!(at("saved") < at("clip_moved"), "saved is reported before the move");
    assert!(all.iter().any(|l| l.contains("save timing (staged)")), "{all:?}");
    assert!(all.iter().any(|l| l.contains("move timing (copy)")), "{all:?}");

    assert_eq!(std::fs::read(&final_path).unwrap(), vec![7u8; 50_000]);
    let left: Vec<_> = std::fs::read_dir(&stage).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(left, vec![stage.join("kept")], "staged copy and .part gone from the staging folder");
    assert_eq!(moved["kept_path"], text(&stage.join("kept").join(CLIP)), "the staged copy waits for the app");
    assert!(!part_path(&text(&final_path)).exists());
    assert_eq!(galleries.lock().unwrap().clone(), vec![(text(&final_path), 50_000)], "gallery uses the FINAL path");
    drop(queue);
    assert_eq!(control.shutdown(Duration::from_secs(3)), Shutdown::Drained);
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

#[test]
fn same_volume_saves_in_place_and_start_recovers_leftovers_even_with_staging_off() {
    let (stage, out) = (temp_dir("left"), temp_dir("out2"));
    std::fs::write(stage.join("Old__2026-10-22__09-00-00-000__cd34.mp4"), b"finished earlier").unwrap();
    std::fs::write(stage.join("Cut__2026-10-22__09-01-00-000__ef56.mp4.part"), b"half").unwrap();
    let (events, sink) = Events::capture(true);
    // Staging folder on the "same volume" as the clip folder: written in place.
    let cfg = setup(Some(&stage), &stage, &out, |_, _| true);
    let (queue, control) = spawn_staged(events, fixed_mux(10), no_gallery(), Some(cfg));
    let moved = wait_for(&sink, "clip_moved", 1);
    let old = out.join("Old__2026-10-22__09-00-00-000__cd34.mp4");
    assert_eq!(json(&moved[0])["path"], text(&old));
    assert_eq!(std::fs::read(&old).unwrap(), b"finished earlier");
    let left: Vec<_> = std::fs::read_dir(&stage).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left, vec!["Cut__2026-10-22__09-01-00-000__ef56.mp4.part"], "leftover moved; a cut-off .part is kept, never swept");

    let final_path = out.join(CLIP);
    assert!(queue.submit(job(&final_path)));
    let saved = json(&wait_for(&sink, "saved", 1)[0]);
    assert_eq!(saved["staged"], false);
    assert!(saved.get("staged_path").is_none());
    assert_eq!(std::fs::read(&final_path).unwrap(), vec![7u8; 10]);
    drop(queue);
    assert_eq!(control.shutdown(Duration::from_secs(3)), Shutdown::Drained);

    // Staging off: leftovers in the (default) folder still move at start.
    std::fs::write(stage.join("Late__2026-10-22__09-02-00-000__aa11.mp4"), b"x").unwrap();
    let (events, sink) = Events::capture(true);
    let (queue, control) = spawn_staged(events, fixed_mux(10), no_gallery(), Some(setup(None, &stage, &out, |_, _| false)));
    wait_for(&sink, "clip_moved", 1);
    assert!(out.join("Late__2026-10-22__09-02-00-000__aa11.mp4").exists());
    drop(queue);
    assert_eq!(control.shutdown(Duration::from_secs(3)), Shutdown::Drained);
    let _ = (std::fs::remove_dir_all(&stage), std::fs::remove_dir_all(&out));
}

// ---- status.save_writing (R4-overlay-1) ----

#[test]
fn the_save_being_written_reports_its_growing_part_and_the_queue_behind_it() {
    let dir = temp_dir("writing");
    let (events, _sink) = Events::capture(true);
    let (wrote_tx, wrote_rx) = channel::<()>();
    let (gate_tx, gate_rx) = channel::<()>();
    // Writes the `.part` in three steps, waiting for the test between them.
    let mux: MuxFn = Box::new(move |part, _job, _abort, _timing| {
        for n in 1..=3usize {
            std::fs::write(part, vec![7u8; n * 1000]).unwrap();
            super::super::save_progress::wrote(1000); // the mux counts what it writes
            wrote_tx.send(()).unwrap();
            gate_rx.recv().unwrap();
        }
        Ok(Muxed { frames: 1, secs: 1.5, partial: None })
    });
    let (queue, control) = spawn(events, mux, no_gallery());
    let wait = Duration::from_secs(5);
    assert_eq!(control.writing(), None, "nothing in the saver");
    assert!(queue.submit(job(&dir.join("a.mp4"))));
    wrote_rx.recv_timeout(wait).unwrap();
    assert_eq!(control.writing().map(|w| (w.bytes, w.queued)), Some((1000, 0)));
    assert!(queue.submit(job(&dir.join("b.mp4"))));
    gate_tx.send(()).unwrap();
    wrote_rx.recv_timeout(wait).unwrap();
    assert_eq!(control.writing().map(|w| (w.bytes, w.queued)), Some((2000, 1)), "the bytes grow; b waits");
    assert!(queue.submit(job(&dir.join("c.mp4"))));
    assert!(!queue.submit(job(&dir.join("d.mp4"))), "queue full: rejected");
    assert_eq!(control.writing().map(|w| w.queued), Some(2), "a rejected save is not counted");
    for _ in 0..7 {
        gate_tx.send(()).unwrap();
        let _ = wrote_rx.recv_timeout(wait);
    }
    gate_tx.send(()).unwrap();
    let deadline = Instant::now() + wait;
    while control.writing().is_some() {
        assert!(Instant::now() < deadline, "the saver never went idle: {:?}", control.writing());
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(queue);
    assert_eq!(control.shutdown(Duration::from_secs(3)), Shutdown::Drained);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_saver_reports_its_steps_before_any_file_exists() {
    // R5-overlay-4: the audio encode runs before the `.part` is opened; its frames count.
    let dir = temp_dir("steps");
    let (events, _sink) = Events::capture(true);
    let (stepped_tx, stepped_rx) = channel::<()>();
    let (gate_tx, gate_rx) = channel::<()>();
    let mux: MuxFn = Box::new(move |_part, _job, _abort, _timing| {
        for _ in 0..3 {
            super::super::save_progress::step(); // an AAC frame, no file written
            stepped_tx.send(()).unwrap();
            gate_rx.recv().unwrap();
        }
        Err("stopped by the test".into())
    });
    let (queue, control) = spawn(events, mux, no_gallery());
    let wait = Duration::from_secs(5);
    assert!(queue.submit(job(&dir.join("a.mp4"))));
    let mut steps = Vec::new();
    for _ in 0..3 {
        stepped_rx.recv_timeout(wait).unwrap();
        let w = control.writing().expect("a save is in the saver");
        assert_eq!(w.bytes, 0, "no packet muxed yet");
        steps.push(w.step);
        gate_tx.send(()).unwrap();
    }
    assert!(steps.windows(2).all(|p| p[1] > p[0]), "the step rises with the encode: {steps:?}");
    drop(queue);
    assert_eq!(control.shutdown(Duration::from_secs(3)), Shutdown::Drained);
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Sort clips into game folders", on the saver thread: a missing game folder is made with its
/// marker and the file name is kept; no game leaves the path alone; a missing clip folder is not
/// made; a game folder that is a link is never written through.
#[test]
fn the_saver_places_a_clip_in_its_game_folder_and_never_through_a_link() {
    use crate::game_folder_rule as rule;
    let dir = temp_dir("gamefolder");
    let (events, _sink) = Events::capture(true);
    let out = text(&dir);
    let file = "Rocket-League__2026-10-22__14-05-09-275__ab.mp4";
    let flat = text(&dir.join(file));
    assert_eq!(place_in_game_folder(&flat, Some("Rocket League®"), &out, &events), text(&dir.join("Rocket League").join(file)));
    assert!(rule::is_game_folder(&dir.join("Rocket League")));
    assert_eq!(place_in_game_folder(&flat, None, &out, &events), flat, "game folders off");
    let lost = dir.join("gone");
    let lost_clip = text(&lost.join("G__2026-10-22__14-05-09-275__ab.mp4"));
    assert_eq!(place_in_game_folder(&lost_clip, Some("Game"), &text(&lost), &events), lost_clip);
    assert!(!lost.exists(), "a missing clip folder is never made");
    let target = dir.join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join(rule::MARKER), b"").unwrap();
    if !rule::junction_for_test(&target, &dir.join("Apex Legends")) {
        eprintln!("skipped: could not make a junction");
        return;
    }
    let apex = "Apex-Legends__2026-10-22__14-05-09-275__ab.mp4";
    let placed = place_in_game_folder(&text(&dir.join(apex)), Some("Apex Legends"), &out, &events);
    assert_eq!(placed, text(&dir.join("Apex Legends (Recall)").join(apex)));
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1, "nothing written through the link");
}

/// Gate fix: the marked game folder is deleted mid-run and the user makes their own folder with
/// the same name. The cached folder is not Recall's now: the clip goes into the clip folder under
/// the same name, no marker is written, and the next save looks again.
#[test]
fn a_game_folder_replaced_mid_run_sends_the_clip_to_the_clip_folder() {
    use crate::game_folder_rule as rule;
    let dir = temp_dir("replaced");
    let (events, sink) = Events::capture(false);
    let out = text(&dir);
    let file = "VALORANT__2026-11-10__04-00-00-000__ab.mp4";
    let game = dir.join("VALORANT");
    assert_eq!(place_in_game_folder(&text(&dir.join(file)), Some("VALORANT"), &out, &events), text(&game.join(file)));
    std::fs::remove_dir_all(&game).unwrap();
    std::fs::create_dir_all(&game).unwrap();
    std::fs::write(game.join("mine.txt"), b"user").unwrap();
    let again = place_in_game_folder(&text(&dir.join(file)), Some("VALORANT"), &out, &events);
    assert_eq!(again, text(&dir.join(file)), "saved straight in the clip folder");
    assert!(!rule::has_marker(&game), "the user's folder gets no marker");
    assert_eq!(std::fs::read_dir(&game).unwrap().count(), 1, "nothing else touched");
    let lines = sink.lock().unwrap().clone();
    assert!(lines.iter().any(|l| l.contains("is not Recall's game folder now")), "{lines:?}");
    let next = place_in_game_folder(&text(&dir.join(file)), Some("VALORANT"), &out, &events);
    // The next save resolves again: now it is the user's folder, with no answer yet (asked once).
    assert_eq!(next, text(&dir.join(file)), "the next save resolves again");
    assert!(!rule::has_marker(&game));
}

#[path = "saver_game_tests.rs"]
mod game;
