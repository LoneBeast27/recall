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
//! **Crash recovery.** Fragments are only ever written whole, so a `.part`
//! left by a crash or a kill plays up to its last flushed fragment (at most a
//! few seconds short). The next engine start renames every
//! `*__rec.mp4.part` in `out_dir` to `*__rec.mp4` (or `*-recovered.mp4` if
//! that name is taken) and emits `recording_recovered{path}`. A file still
//! open by a running recorder cannot be renamed (it is opened without
//! delete-sharing), so recovery never touches a live recording.

mod fmp4;
pub(crate) mod machine;
pub mod tap;
#[cfg(test)]
pub(crate) mod test_sink;

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
    /// The source's gain is above 0 dB: the recording limits this track to -1 dBFS.
    pub boosted: bool,
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
        Writer { out_dir, json, recorder: Recorder::new(fmp4::Fmp4Factory, tracks), generation: tap::generation(), skew: Duration::ZERO }
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
    /// Added to the clock (tests step time with it; zero live).
    skew: Duration,
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
                let mut events = self.encoder_seen(generation);
                if generation == self.generation {
                    let gap = tap::take_gap();
                    events.extend(self.recorder.video(pts_us, keyframe, data, gap, self.now()));
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
    /// stream's parameters: a running part ends here and the next one gets that
    /// generation's settings (older packets are the caller's to skip). If the
    /// old encoder failed, the recording ends with it, whichever is seen first.
    fn encoder_seen(&mut self, generation: u64) -> Vec<Event> {
        if generation <= self.generation {
            return Vec::new();
        }
        let failed = tap::failure_of(self.generation);
        self.generation = generation;
        if let Some(reason) = failed {
            return self.recorder.encoder_failed(&reason);
        }
        let (video, cause) = match self.recorder.factory().stream_of(generation) {
            Some((video, cause)) => (Some(video), cause),
            None => (None, machine::Cause::Encoder),
        };
        self.recorder.encoder_reopened(self.now(), cause, video)
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
            let started = Outbound::RecordingStarted { path: path.clone(), continued: false, part: 1, cause: None };
            emit_or_log(json, &started, || format!("long recording -> {path}"));
        }
        Event::PartStarted { path, part, cause } => {
            let path = path_text(&path);
            let started = Outbound::RecordingStarted { path: path.clone(), continued: true, part, cause: Some(cause.as_str()) };
            emit_or_log(json, &started, || format!("long recording continues in part {part} ({}) -> {path}", cause.as_str()));
        }
        Event::Stopped { path, seconds, bytes, marks, parts, total_seconds } => {
            stopped(json, &path, Stop { seconds, bytes, marks, parts, total_seconds, cause: None })
        }
        Event::Continued { path, seconds, bytes, marks, part, total_seconds, cause } => {
            stopped(json, &path, Stop { seconds, bytes, marks, parts: part, total_seconds, cause: Some(cause.as_str()) })
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

/// One `recording_stopped`. `cause`: only this part ended (the stream changed).
struct Stop {
    seconds: f64,
    bytes: u64,
    marks: usize,
    parts: u32,
    total_seconds: f64,
    cause: Option<&'static str>,
}

fn stopped(json: bool, path: &Path, stop: Stop) {
    let path = path.to_string_lossy().into_owned();
    let Stop { seconds, bytes, marks, parts, total_seconds, cause } = stop;
    let out = Outbound::RecordingStopped { path: path.clone(), seconds, bytes, continued: cause.is_some(), parts, total_seconds, cause };
    emit_or_log(json, &out, || {
        let what = match cause {
            Some(cause) => format!("part {parts} saved ({cause} changed; continuing in a new file)"),
            None => format!("saved ({parts} file(s), {total_seconds:.1} s in all)"),
        };
        format!("long recording {what} {path} ({seconds:.1}s, {bytes} bytes, {marks} marks)")
    });
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
#[path = "longrec_tests.rs"]
mod tests;
