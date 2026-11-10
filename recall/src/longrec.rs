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
//! of a reopened encoder (new stream parameters) ends the current part and the
//! recording continues in the next file (`machine_parts.rs`) even if the
//! `EncoderReopened` notice was dropped; stale packets of an older encoder are
//! skipped. Each part opens with its own generation's settings
//! (`tap::stream_of`), never a newer encoder's. A live encoder that fails
//! (`tap::on_encoder_failed`, polled, never dropped) finishes the recording
//! and reports `recording_failed` naming the saved file, also when it has
//! already reopened.
//!
//! **Crash recovery** (`recover.rs`). Fragments are only ever written whole, so a `.part`
//! left by a crash or a kill plays up to its last flushed fragment (at most a few seconds
//! short). Every part is listed in the in-progress list (`encoder::in_progress`) while it is
//! written; the next engine start repairs each listed one (`restore.rs`: cut to its last whole
//! fragment, its length set), renames it to `*__rec.mp4` (or `*-recovered.mp4`), and emits
//! `recording_restored` per file and `recording_recovered{paths}`. The app then offers
//! Resume recording (`record_start{continues}`: the next part) or End it here; recovery never
//! starts a recording. A file still open by a running recorder is never touched.

mod fmp4;
mod lock_clock;
pub(crate) mod machine;
pub(crate) mod recover;
pub(crate) mod restore;
pub use recover::recover_parts;
#[cfg(test)]
use recover::{recover_in, recovered_outbound};
pub mod tap;
#[cfg(test)]
pub(crate) mod test_sink;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::naming;
use crate::protocol::{emit_or_log, Outbound};
use lock_clock::LockClock;
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
    /// `at`: when the encoder reopened (the next part's rate cap counts from it).
    EncoderReopened { generation: u64, at: Instant },
}

/// A start's (capture-clock µs, `Instant` it was read at), sampled together
/// on the command thread (`CaptureShared::recording_now`).
pub type StartClock = Option<(u64, Instant)>;

enum Control {
    /// `continues`: a recovered recording this one goes on from (its next part).
    Start { source: String, start: StartClock, continues: Option<PathBuf> },
    Stop,
    Toggle { source: String, start: StartClock },
    Mark { at_us: u64 },
    /// End a running recording as failed (every part kept, `recording_failed`).
    Fail { reason: String },
    /// The session locked / unlocked (`lock_clock`).
    Locked(bool),
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
        Writer { out_dir, json, recorder: Recorder::new(fmp4::Fmp4Factory, tracks), generation: tap::generation(), skew: Duration::ZERO, parts_so_far: Vec::new(), missed: (0.0, 0.0), lock: LockClock::default() }
            .run(&ctl_rx, &data_rx);
    });
    if let Err(e) = spawned {
        report(json, Event::Log(format!("long recording thread failed to start: {e}")));
    }
    LongRec { ctl, done: std::sync::Mutex::new(done) }
}

impl LongRec {
    pub fn start(&self, source: &str, start: StartClock, continues: Option<&str>) {
        let _ = self.ctl.send(Control::Start { source: source.to_string(), start, continues: continues.map(PathBuf::from) });
    }

    pub fn stop(&self) {
        let _ = self.ctl.send(Control::Stop);
    }

    pub fn toggle(&self, source: &str, start: StartClock) {
        let _ = self.ctl.send(Control::Toggle { source: source.to_string(), start });
    }

    /// End a running recording as failed now (`record_fail`): the writer thread finishes
    /// it whatever the encoder does, so a hung encoder cannot hold the file open.
    pub fn fail(&self, reason: &str) {
        let _ = self.ctl.send(Control::Fail { reason: reason.to_string() });
    }

    /// A bookmark at capture time `at_us` (kept if a recording is running).
    pub fn mark(&self, at_us: u64) {
        let _ = self.ctl.send(Control::Mark { at_us });
    }

    /// The Windows session locked or unlocked (`session_locked`).
    pub fn set_locked(&self, locked: bool) {
        let _ = self.ctl.send(Control::Locked(locked));
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
    /// Added to the clock (tests step time with it; zero live).
    skew: Duration,
    /// The finished parts of the running recording (a failure names them all).
    parts_so_far: Vec<PathBuf>,
    /// (gap, frozen) seconds of the running recording as of the last step (a failure reports them).
    missed: (f64, f64),
    /// Time locked during the running recording (`recording_stopped.locked_seconds`).
    lock: LockClock,
}

impl<F: SinkFactory> Writer<F> {
    fn now(&self) -> Instant {
        Instant::now() + self.skew
    }

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
            let events = self.recorder.tick(self.now());
            self.events(events);
        }
    }

    fn control(&mut self, c: Control, data: &Receiver<Data>) {
        match c {
            Control::Start { source, start, continues } => self.start(&source, start, continues.as_deref()),
            Control::Stop => self.stop(data),
            Control::Toggle { source, start } => {
                if self.recorder.is_active() {
                    self.stop(data);
                } else {
                    self.start(&source, start, None);
                }
            }
            Control::Mark { at_us } => self.recorder.mark(at_us),
            Control::Fail { reason } => self.fail(&reason, data),
            Control::Locked(locked) => self.lock.set_locked(locked, self.now()),
            Control::Shutdown => {}
        }
    }

    fn start(&mut self, source: &str, start: StartClock, continues: Option<&Path>) {
        if self.recorder.is_active() {
            return self.events(vec![Event::Log("record_start ignored: a long recording is already running".into())]);
        }
        let mut notes = Vec::new();
        let next = continues.and_then(|c| recover::next_part_of(c, &mut notes));
        self.events(notes);
        if let Some(path) = next {
            return self.arm(path, start);
        }
        // Sort clips into game folders: `<out>\<Game>\` when on, checked here on the
        // long-recording thread (never the hotkey); a folder that is not Recall's game folder now
        // falls back to `<out>` (`naming::writable_game_dir`).
        let notes = std::cell::RefCell::new(Vec::new());
        let dir = naming::clip_dir(&self.out_dir, source, &|line: &str| notes.borrow_mut().push(Event::Log(format!("record: {line}"))));
        self.events(notes.into_inner());
        let path = naming::longrec_path(&dir, source, &chrono::Local::now(), Path::exists);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return self.events(vec![Event::Failed { reason: format!("output folder {dir}: {e}"), kept: None }]);
        }
        self.arm(path, start);
    }

    fn arm(&mut self, path: PathBuf, start: StartClock) {
        let (generation, video) = self.recorder.factory().newest();
        self.recorder.set_video(video);
        let events = self.recorder.start(path, start, self.now());
        self.generation = generation;
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
                let mut events = self.encoder_seen(generation, None);
                if generation == self.generation {
                    let gap = tap::take_gap();
                    events.extend(self.recorder.video(pts_us, keyframe, data, gap, self.now()));
                }
                events
            }
            Data::Audio { track, samples, at } => {
                let events = match self.recorder.track_index(track) {
                    Some(i) => self.recorder.audio(i, &samples, at),
                    None => Vec::new(),
                };
                tap::recycle(samples); // the capture thread reuses it
                events
            }
            Data::EncoderReopened { generation, at } => self.encoder_seen(generation, Some(at + self.skew)),
        };
        self.events(events);
    }

    /// An item from live encoder `generation`. A newer one changed the
    /// stream's parameters: a running part ends here and the next one gets that
    /// generation's settings (older packets are the caller's to skip). If the
    /// old encoder failed, the recording ends with it, whichever is seen first.
    /// `at`: when it reopened (None: seen from its first packet, now).
    fn encoder_seen(&mut self, generation: u64, at: Option<Instant>) -> Vec<Event> {
        if generation <= self.generation {
            return Vec::new();
        }
        let failed = tap::failure_of(self.generation);
        let errored = tap::error_of(self.generation);
        self.generation = generation;
        if let Some(reason) = failed {
            return self.recorder.encoder_failed(&reason);
        }
        // The old encoder's encode error, on the writer's clock as the reopen's `at` is.
        if let Some(at) = errored {
            self.recorder.encode_error(at + self.skew);
        }
        let (video, cause) = match self.recorder.factory().stream_of(generation) {
            Some((video, cause)) => (Some(video), cause),
            None => (None, machine::Cause::Encoder),
        };
        self.recorder.encoder_reopened(at.unwrap_or_else(|| self.now()), cause, video)
    }

    /// End the running recording as failed (`record_fail`): what is queued is written
    /// first, then it is finished, every part named in `recording_failed`.
    fn fail(&mut self, reason: &str, data: &Receiver<Data>) {
        if !self.recorder.is_active() {
            return self.events(vec![Event::Log("record_fail ignored: no long recording is running".into())]);
        }
        tap::set_active(false);
        while let Ok(item) = data.try_recv() {
            self.data(item);
        }
        let events = self.recorder.encoder_failed(reason);
        self.events(events);
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
            match e {
                Event::Started { .. } => {
                    self.parts_so_far.clear();
                    self.lock.started(self.now());
                }
                Event::Stopped { path, seconds, bytes, marks, parts, total_seconds, gap_seconds, frozen_seconds } => {
                    self.parts_so_far.clear();
                    let locked_seconds = self.lock.stopped(self.now());
                    stopped(self.json, &path, Stop { seconds, bytes, marks, parts, total_seconds, gap_seconds, frozen_seconds, locked_seconds, cause: None });
                    continue;
                }
                Event::Continued { ref path, .. } => self.parts_so_far.push(path.clone()),
                Event::Failed { reason, kept } => {
                    let files = std::mem::take(&mut self.parts_so_far);
                    report_failed(self.json, &reason, kept.as_deref(), &files, self.missed);
                    continue;
                }
                _ => {}
            }
            report(self.json, e);
        }
        if self.recorder.is_active() {
            self.missed = self.recorder.missed();
        } else {
            tap::set_active(false);
        }
    }
}

fn report(json: bool, event: Event) {
    let path_text = |p: &PathBuf| p.to_string_lossy().into_owned();
    match event {
        Event::Started { path } => {
            let path = path_text(&path);
            let started = Outbound::RecordingStarted { path: path.clone(), continued: false, part: 1, cause: None };
            emit_or_log(json, &started, || format!("long recording -> {path}"));
        }
        Event::PartStarted { path, part, cause } => {
            let path = path_text(&path);
            let started = Outbound::RecordingStarted { path: path.clone(), continued: true, part, cause: Some(cause.as_str()) };
            emit_or_log(json, &started, || format!("long recording continues in part {part} ({}) -> {path}", cause.as_str()));
        }
        // `Writer::events` reports the final stop itself (it adds the locked seconds).
        Event::Stopped { .. } => {}
        Event::Continued { path, seconds, bytes, marks, part, total_seconds, cause } => {
            stopped(json, &path, Stop { seconds, bytes, marks, parts: part, total_seconds, gap_seconds: 0.0, frozen_seconds: 0.0, locked_seconds: 0.0, cause: Some(cause.as_str()) })
        }
        Event::Failed { reason, kept } => report_failed(json, &reason, kept.as_deref(), &[], (0.0, 0.0)),
        Event::Log(message) => emit_or_log(json, &Outbound::EncoderLog { message: message.clone() }, || message),
    }
}

/// `recording_failed`: `path` is the file kept from the part that failed, `paths` every
/// file the recording left (its finished parts first), each once: a failure while the next
/// part waits keeps the finished part, which is already among `earlier`. `missed`: the
/// recording's (gap, frozen) seconds.
fn failed_outbound(reason: &str, kept: Option<&Path>, earlier: &[PathBuf], missed: (f64, f64)) -> Outbound {
    let text = |p: &Path| p.to_string_lossy().into_owned();
    let mut paths: Vec<String> = earlier.iter().map(|p| text(p)).collect();
    let last = kept.map(text).filter(|k| !paths.contains(k));
    paths.extend(last);
    let (gap_seconds, frozen_seconds) = missed;
    Outbound::RecordingFailed { reason: reason.to_string(), path: kept.map(text), paths, gap_seconds, frozen_seconds }
}

fn report_failed(json: bool, reason: &str, kept: Option<&Path>, earlier: &[PathBuf], missed: (f64, f64)) {
    let out = failed_outbound(reason, kept, earlier, missed);
    let Outbound::RecordingFailed { paths, .. } = &out else { return };
    let files = if paths.is_empty() { "nothing".to_string() } else { paths.join(", ") };
    emit_or_log(json, &out, || format!("long recording FAILED: {reason} (kept: {files})"));
}

/// One `recording_stopped`. `cause`: only this part ended (the stream changed).
struct Stop {
    seconds: f64,
    bytes: u64,
    marks: usize,
    parts: u32,
    total_seconds: f64,
    gap_seconds: f64,
    frozen_seconds: f64,
    locked_seconds: f64,
    cause: Option<&'static str>,
}

fn stopped_outbound(path: &str, stop: Stop) -> Outbound {
    let Stop { seconds, bytes, marks: _, parts, total_seconds, gap_seconds, frozen_seconds, locked_seconds, cause } = stop;
    Outbound::RecordingStopped { path: path.to_string(), seconds, bytes, continued: cause.is_some(), parts, total_seconds, gap_seconds, frozen_seconds, locked_seconds, cause }
}

fn stopped(json: bool, path: &Path, stop: Stop) {
    let path = path.to_string_lossy().into_owned();
    let (seconds, bytes, marks, parts, total_seconds, gap_seconds, frozen_seconds, cause) =
        (stop.seconds, stop.bytes, stop.marks, stop.parts, stop.total_seconds, stop.gap_seconds, stop.frozen_seconds, stop.cause);
    let out = stopped_outbound(&path, stop);
    #[cfg(test)]
    tests::LAST_STOP.with(|last| *last.borrow_mut() = serde_json::to_value(&out).ok());
    emit_or_log(json, &out, || {
        let what = match cause {
            Some(cause) => format!("part {parts} saved ({cause} changed; continuing in a new file)"),
            None => format!("saved ({parts} file(s), {total_seconds:.1} s in all, {gap_seconds:.1} s missing between them, {frozen_seconds:.1} s frozen)"),
        };
        format!("long recording {what} {path} ({seconds:.1}s, {bytes} bytes, {marks} marks)")
    });
}

#[cfg(test)]
#[path = "longrec_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "longrec_quit_child_tests.rs"]
mod quit_child_tests;
#[cfg(test)]
#[path = "longrec_restore_tests.rs"]
mod restore_tests;
#[cfg(test)]
#[path = "longrec_journal_tests.rs"]
mod journal_tests;
#[cfg(test)]
#[path = "longrec_review_tests.rs"]
mod review_tests;
/// The app's library length reader, the very file (`recall-native`, std only): restore test 6.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../recall-native/src/services/library/mp4.rs"]
mod library_mp4;
