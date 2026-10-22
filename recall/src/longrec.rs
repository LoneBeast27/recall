//! Manual long recording (record hotkey / `record_start` / `record_stop` /
//! `record_toggle`): the live encoder's packets and every audio track stream
//! to `<out_dir>\<Game>__<date>__<time>__rec.mp4.part`, a fragmented MP4
//! written by the `recall-longrec` thread in large sequential writes, renamed
//! to `...__rec.mp4` on stop.
//!
//! Cost on the live path: see `tap` (one atomic load per packet / audio chunk
//! while idle). The writer is a separate thread with its own bounded queue,
//! so a slow disk drops (and later resyncs) recording data but never stalls
//! capture, encoding or replay saves.
//!
//! Every tapped packet carries its live-encoder generation: the first packet
//! of a reopened encoder (new stream parameters) finishes the recording
//! cleanly even if the `EncoderReopened` notice was dropped; stale packets of
//! an older encoder are skipped. A live encoder that fails for good
//! (`tap::on_encoder_failed`, polled, never dropped) finishes the recording
//! and reports `recording_failed` naming the saved file.
//!
//! **Crash recovery.** Fragments are only ever written whole, so a `.part`
//! left by a crash or a kill plays up to its last flushed fragment (at most a
//! few seconds short). The next engine start renames every
//! `*__rec.mp4.part` in `out_dir` to `*__rec.mp4` (or `*-recovered.mp4` if
//! that name is taken) and emits `recording_recovered{path}`. A file still
//! open by a running recorder cannot be renamed (it is opened without
//! delete-sharing), so recovery never touches a live recording.

mod fmp4;
mod machine;
pub mod tap;
#[cfg(test)]
mod test_sink;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::naming;
use crate::protocol::{emit_or_log, Outbound};
use machine::{Event, Recorder, SinkFactory};

/// Data items the writer may queue (~40 s of 60 fps video + 3 audio tracks).
const DATA_QUEUE: usize = 16_384;
const IDLE_WAIT: Duration = Duration::from_millis(100);
const ACTIVE_WAIT: Duration = Duration::from_millis(20);

/// One audio track as the engine captures it.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackInfo {
    pub name: &'static str,
    pub rate: u32,
    pub channels: u16,
}

/// Live-path items (from `tap`).
pub(crate) enum Data {
    /// `generation`: the live encoder that produced it (`tap::generation`).
    Video { pts_us: u64, keyframe: bool, data: Arc<[u8]>, generation: u64 },
    Audio { track: &'static str, samples: Vec<f32>, at: Instant },
    EncoderReopened { generation: u64 },
}

/// A start's (capture-clock µs, `Instant` it was read at), sampled together
/// on the command thread (`CaptureShared::recording_now`).
pub type StartClock = Option<(u64, Instant)>;

enum Control {
    Start { source: String, start: StartClock },
    Stop,
    Toggle { source: String, start: StartClock },
    Mark { at_us: u64 },
    Shutdown,
}

/// Handle owned by the engine (stdin thread commands, quit-time shutdown).
pub struct LongRec {
    ctl: Sender<Control>,
    done: std::sync::Mutex<Receiver<()>>,
}

/// Start the writer thread for recordings into `out_dir` with these tracks.
pub fn spawn(out_dir: String, json: bool, tracks: Vec<TrackInfo>) -> LongRec {
    let (data_tx, data_rx) = sync_channel::<Data>(DATA_QUEUE);
    tap::install_sender(data_tx);
    let (ctl, ctl_rx) = channel::<Control>();
    let (done_tx, done) = channel::<()>();
    let spawned = std::thread::Builder::new().name("recall-longrec".into()).spawn(move || {
        let _done = done_tx; // dropped when the thread ends
        Writer { out_dir, json, recorder: Recorder::new(fmp4::Fmp4Factory, tracks), generation: tap::generation() }
            .run(&ctl_rx, &data_rx);
    });
    if let Err(e) = spawned {
        report(json, Event::Log(format!("long recording thread failed to start: {e}")));
    }
    LongRec { ctl, done: std::sync::Mutex::new(done) }
}

impl LongRec {
    pub fn start(&self, source: &str, start: StartClock) {
        let _ = self.ctl.send(Control::Start { source: source.to_string(), start });
    }

    pub fn stop(&self) {
        let _ = self.ctl.send(Control::Stop);
    }

    pub fn toggle(&self, source: &str, start: StartClock) {
        let _ = self.ctl.send(Control::Toggle { source: source.to_string(), start });
    }

    /// A bookmark at capture time `at_us` (kept if a recording is running).
    pub fn mark(&self, at_us: u64) {
        let _ = self.ctl.send(Control::Mark { at_us });
    }

    /// Quit path: finalize a running recording within `budget`. Past it the
    /// process may exit; the `.part` is then recovered on the next start.
    pub fn shutdown(&self, budget: Duration) {
        let _ = self.ctl.send(Control::Shutdown);
        let _ = self.done.lock().map(|d| d.recv_timeout(budget));
    }
}

struct Writer<F: SinkFactory> {
    out_dir: String,
    json: bool,
    recorder: Recorder<F>,
    /// The live-encoder generation the current recording's packets come from.
    generation: u64,
}

impl<F: SinkFactory> Writer<F> {
    fn run(mut self, ctl: &Receiver<Control>, data: &Receiver<Data>) {
        loop {
            loop {
                match ctl.try_recv() {
                    Ok(Control::Shutdown) => {
                        if self.recorder.is_active() {
                            self.stop(data);
                        }
                        return;
                    }
                    Ok(c) => self.control(c, data),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        if self.recorder.is_active() {
                            self.stop(data);
                        }
                        return;
                    }
                }
            }
            let wait = if self.recorder.is_active() { ACTIVE_WAIT } else { IDLE_WAIT };
            match data.recv_timeout(wait) {
                Ok(item) => {
                    self.data(item);
                    while let Ok(item) = data.try_recv() {
                        self.data(item);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.check_encoder_failed(data);
            let events = self.recorder.tick(Instant::now());
            self.events(events);
        }
    }

    fn control(&mut self, c: Control, data: &Receiver<Data>) {
        match c {
            Control::Start { source, start } => self.start(&source, start),
            Control::Stop => self.stop(data),
            Control::Toggle { source, start } => {
                if self.recorder.is_active() {
                    self.stop(data);
                } else {
                    self.start(&source, start);
                }
            }
            Control::Mark { at_us } => self.recorder.mark(at_us),
            Control::Shutdown => {}
        }
    }

    fn start(&mut self, source: &str, start: StartClock) {
        if self.recorder.is_active() {
            return self.events(vec![Event::Log("record_start ignored: a long recording is already running".into())]);
        }
        let path = naming::longrec_path(&self.out_dir, source, &chrono::Local::now(), Path::exists);
        if let Err(e) = std::fs::create_dir_all(&self.out_dir) {
            return self.events(vec![Event::Failed { reason: format!("output folder {}: {e}", self.out_dir), kept: None }]);
        }
        let events = self.recorder.start(path, start, Instant::now());
        self.generation = tap::generation();
        tap::set_active(true);
        self.events(events);
    }

    /// Process what the live path already queued, then finalize.
    fn stop(&mut self, data: &Receiver<Data>) {
        tap::set_active(false);
        while let Ok(item) = data.try_recv() {
            self.data(item);
        }
        let events = self.recorder.stop();
        self.events(events);
    }

    fn data(&mut self, item: Data) {
        let events = match item {
            Data::Video { pts_us, keyframe, data, generation } => {
                let mut events = self.encoder_seen(generation);
                if generation == self.generation {
                    let gap = tap::take_gap();
                    events.extend(self.recorder.video(pts_us, keyframe, data, gap, Instant::now()));
                }
                events
            }
            Data::Audio { track, samples, at } => match self.recorder.track_index(track) {
                Some(i) => self.recorder.audio(i, &samples, at),
                None => Vec::new(),
            },
            Data::EncoderReopened { generation } => self.encoder_seen(generation),
        };
        self.events(events);
    }

    /// An item from live encoder `generation`. A newer one changed the
    /// stream's parameters: a running recording finishes here (older packets
    /// are the caller's to skip).
    fn encoder_seen(&mut self, generation: u64) -> Vec<Event> {
        if generation <= self.generation {
            return Vec::new();
        }
        self.generation = generation;
        self.recorder.encoder_reopened()
    }

    /// The live encoder of this recording died for good: write what it
    /// already queued, then finish and report the recording.
    fn check_encoder_failed(&mut self, data: &Receiver<Data>) {
        if !self.recorder.is_active() {
            return;
        }
        let Some(reason) = tap::failure_of(self.generation) else { return };
        tap::set_active(false);
        while let Ok(item) = data.try_recv() {
            self.data(item);
        }
        let events = self.recorder.encoder_failed(&reason);
        self.events(events);
    }

    fn events(&mut self, events: Vec<Event>) {
        for e in events {
            report(self.json, e);
        }
        if !self.recorder.is_active() {
            tap::set_active(false);
        }
    }
}

fn report(json: bool, event: Event) {
    let path_text = |p: &PathBuf| p.to_string_lossy().into_owned();
    match event {
        Event::Started { path } => {
            let path = path_text(&path);
            emit_or_log(json, &Outbound::RecordingStarted { path: path.clone() }, || format!("long recording -> {path}"));
        }
        Event::Stopped { path, seconds, bytes, marks } => {
            let path = path_text(&path);
            emit_or_log(json, &Outbound::RecordingStopped { path: path.clone(), seconds, bytes }, || {
                format!("long recording saved {path} ({seconds:.1}s, {bytes} bytes, {marks} marks)")
            });
        }
        Event::Failed { reason, kept } => {
            let kept = kept.as_ref().map(path_text);
            emit_or_log(json, &Outbound::RecordingFailed { reason: reason.clone(), path: kept.clone() }, || {
                format!("long recording FAILED: {reason} (kept: {})", kept.as_deref().unwrap_or("nothing"))
            });
        }
        Event::Log(message) => emit_or_log(json, &Outbound::EncoderLog { message: message.clone() }, || message),
    }
}

/// Rename long recordings a crash left as `.part` (background thread: the
/// clip folder may be a sleeping HDD). See the module docs.
pub fn recover_parts(out_dir: String, json: bool) {
    let _ = std::thread::Builder::new().name("recall-longrec-recover".into()).spawn(move || {
        for (part, result) in recover_in(Path::new(&out_dir)) {
            match result {
                Ok(path) => {
                    let path = path.to_string_lossy().into_owned();
                    emit_or_log(json, &Outbound::RecordingRecovered { path: path.clone() }, || format!("recovered long recording {path}"));
                }
                Err(e) => report(json, Event::Log(format!("long recording {} not recovered: {e}", part.display()))),
            }
        }
    });
}

/// Every `*__rec.mp4.part` directly in `dir`, renamed (or the error).
fn recover_in(dir: &Path) -> Vec<(PathBuf, std::io::Result<PathBuf>)> {
    let Ok(listing) = std::fs::read_dir(dir) else { return Vec::new() };
    listing
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| naming::is_longrec_part(&e.file_name().to_string_lossy()))
        .map(|e| {
            let part = e.path();
            let target = naming::recovered_path(&part, Path::exists);
            let result = std::fs::rename(&part, &target).map(|_| target);
            (part, result)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::mpsc::sync_channel;
    use test_sink::{pkt, temp_dir, tracks, FakeFactory, MS};

    fn writer(tag: &str, generation: u64) -> (Writer<FakeFactory>, PathBuf) {
        let dir = temp_dir(tag);
        let recorder = Recorder::new(FakeFactory::default(), tracks());
        (Writer { out_dir: dir.to_string_lossy().into_owned(), json: false, recorder, generation }, dir)
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
    fn a_reopened_encoders_first_packet_ends_the_recording_without_the_notice() {
        let (mut w, dir) = writer("reopen", 0);
        let (_tx, rx) = sync_channel::<Data>(4);
        w.start("G", Some((0, Instant::now())));
        w.generation = 3; // the encoder generation the recording started on
        w.data(video(0, true, 3));
        w.data(video(16, false, 3));
        assert!(w.recorder.is_active());
        w.data(video(20, false, 2)); // a stale packet of an older encoder: skipped
        assert!(w.recorder.is_active());
        // The EncoderReopened notice was dropped (queue full); the new
        // encoder's first keyframe still ends the file cleanly.
        w.data(video(33, true, 4));
        assert!(!w.recorder.is_active(), "the recording ended at the stream change");
        assert_eq!(w.generation, 4);
        assert_eq!(finished(&dir).len(), 1, "finished and renamed into place");
        w.check_encoder_failed(&rx); // idle: nothing to do
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_encoder_ends_the_recording_after_its_queued_packets() {
        // A generation no other test's writer uses (the failure is global).
        let generation = u64::MAX - 7;
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
}
