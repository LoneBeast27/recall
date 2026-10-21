//! Long-recording state machine against a recording fake sink (no FFmpeg).

use super::*;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Open(PathBuf),
    Video { pts: u64, dur: u64, key: bool, tag: u8 },
    Audio { track: usize, frames: usize },
    Silence { track: usize, frames: u64 },
    Finish,
    Abandon,
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Call>>>);
impl Log {
    fn push(&self, c: Call) {
        self.0.lock().unwrap().push(c);
    }
    fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().clone()
    }
    fn videos(&self) -> Vec<(u64, u64, bool, u8)> {
        self.calls().into_iter().filter_map(|c| match c {
            Call::Video { pts, dur, key, tag } => Some((pts, dur, key, tag)),
            _ => None,
        }).collect()
    }
    /// Frames per track in call order: positive = audio, negative = silence.
    fn audio(&self, track: usize) -> Vec<i64> {
        self.calls().into_iter().filter_map(|c| match c {
            Call::Audio { track: t, frames } if t == track => Some(frames as i64),
            Call::Silence { track: t, frames } if t == track => Some(-(frames as i64)),
            _ => None,
        }).collect()
    }
}

struct FakeSink {
    log: Log,
    part: PathBuf,
    fail_video_after: Option<usize>,
    videos: usize,
}

impl Sink for FakeSink {
    fn video(&mut self, pts_us: u64, duration_us: u64, keyframe: bool, data: &[u8]) -> Result<(), String> {
        if self.fail_video_after.is_some_and(|n| self.videos >= n) {
            return Err("disk full".into());
        }
        self.videos += 1;
        self.log.push(Call::Video { pts: pts_us, dur: duration_us, key: keyframe, tag: data[0] });
        Ok(())
    }
    fn audio(&mut self, track: usize, samples: &[f32]) -> Result<(), String> {
        self.log.push(Call::Audio { track, frames: samples.len() / 2 });
        Ok(())
    }
    fn silence(&mut self, track: usize, frames: u64) -> Result<(), String> {
        self.log.push(Call::Silence { track, frames });
        Ok(())
    }
    fn finish(&mut self) -> Result<u64, String> {
        self.log.push(Call::Finish);
        Ok(std::fs::metadata(&self.part).map(|m| m.len()).unwrap_or(0))
    }
    fn abandon(&mut self) {
        self.log.push(Call::Abandon);
    }
}

struct FakeFactory {
    log: Log,
    fail_open: bool,
    fail_video_after: Option<usize>,
}

impl SinkFactory for FakeFactory {
    type Sink = FakeSink;
    fn open(&mut self, part: &Path, _tracks: &[TrackInfo]) -> Result<FakeSink, String> {
        if self.fail_open {
            return Err("no encoder parameters".into());
        }
        std::fs::write(part, b"fmp4").map_err(|e| e.to_string())?;
        self.log.push(Call::Open(part.to_path_buf()));
        Ok(FakeSink { log: self.log.clone(), part: part.to_path_buf(), fail_video_after: self.fail_video_after, videos: 0 })
    }
}

const MS: u64 = 1_000;

fn tracks() -> Vec<TrackInfo> {
    vec![TrackInfo { name: "Game", rate: 48_000, channels: 2 }, TrackInfo { name: "Mic", rate: 48_000, channels: 2 }]
}

fn recorder(tag: &str) -> (Recorder<FakeFactory>, Log, PathBuf) {
    let dir = std::env::temp_dir().join(format!("recall-longrec-{tag}-{}-{}", std::process::id(), uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = Log::default();
    let rec = Recorder::new(FakeFactory { log: log.clone(), fail_open: false, fail_video_after: None }, tracks());
    (rec, log, dir.join("G__2026-10-22__14-05-09__rec.mp4"))
}

fn pkt(tag: u8) -> Arc<[u8]> {
    Arc::from(vec![tag])
}

fn chunk(frames: usize) -> Vec<f32> {
    vec![0.1; frames * 2]
}

#[test]
fn waits_for_a_keyframe_then_writes_each_packet_one_step_late() {
    let (mut rec, log, path) = recorder("basic");
    let t0 = Instant::now();
    assert!(rec.start(path.clone(), Some(1_000 * MS), t0).is_empty());
    assert!(rec.is_active());
    assert!(rec.video(1_005 * MS, false, pkt(1), false, t0).is_empty(), "P-frame before a keyframe is dropped");
    assert_eq!(rec.video(1_020 * MS, true, pkt(2), false, t0), vec![Event::Started { path: path.clone() }]);
    rec.video(1_036 * MS, false, pkt(3), false, t0);
    rec.video(1_070 * MS, false, pkt(4), false, t0);
    assert_eq!(log.videos(), vec![(0, 16 * MS, true, 2), (16 * MS, 34 * MS, false, 3)], "VFR durations from the next PTS");
    // The tail packet reuses the previous duration: (1070 + 34 - 1020) ms.
    let events = rec.stop();
    assert_eq!(events, vec![Event::Stopped { path: path.clone(), seconds: 0.084, bytes: 4, marks: 0 }]);
    assert_eq!(log.videos().last(), Some(&(50 * MS, 34 * MS, false, 4)));
    assert!(path.is_file(), "renamed into place");
    assert!(!naming::part_of(&path).exists(), "no .part left");
    assert_eq!(log.calls().iter().filter(|c| **c == Call::Finish).count(), 1);
    assert!(!rec.is_active());
}

#[test]
fn dropped_packets_resync_at_the_next_keyframe_and_regressions_are_skipped() {
    let (mut rec, log, path) = recorder("gap");
    let t0 = Instant::now();
    rec.start(path, Some(0), t0);
    rec.video(0, true, pkt(1), false, t0);
    rec.video(16 * MS, false, pkt(2), false, t0);
    rec.video(48 * MS, false, pkt(3), true, t0); // queue overflowed before this one
    rec.video(64 * MS, false, pkt(4), false, t0);
    rec.video(1_000 * MS, true, pkt(5), false, t0);
    rec.video(900 * MS, false, pkt(6), false, t0); // regression
    rec.video(1_016 * MS, false, pkt(7), false, t0);
    let tags: Vec<u8> = log.videos().iter().map(|v| v.3).collect();
    assert_eq!(tags, vec![1, 2, 5], "3 and 4 skipped until keyframe 5; 6 never written");
    assert_eq!(log.videos()[1], (16 * MS, 984 * MS, false, 2), "the held packet spans the gap");
    rec.stop();
    assert_eq!(log.videos().last().unwrap().3, 7);
}

#[test]
fn audio_is_placed_by_wall_clock_padded_and_trimmed() {
    let (mut rec, log, path) = recorder("audio");
    let t0 = Instant::now();
    // Request at capture 10 s; the keyframe is captured 40 ms later.
    rec.start(path, Some(10_000 * MS), t0);
    rec.video(10_040 * MS, true, pkt(1), false, t0 + Duration::from_millis(45));
    let wall_base = t0 + Duration::from_millis(40);
    let at = |ms: u64| wall_base + Duration::from_millis(ms);
    // A chunk ending before the recording starts is dropped entirely.
    rec.audio(0, &chunk(480), wall_base - Duration::from_millis(1));
    // 10 ms chunks ending 15 ms and 25 ms in: continuous (within tolerance).
    rec.audio(0, &chunk(480), at(15));
    rec.audio(0, &chunk(480), at(25));
    // 600 ms of nothing (silent loopback), then a chunk: the gap is padded so
    // the chunk sits at its real-time position (30_000 frames in).
    rec.audio(0, &chunk(480), at(635));
    assert_eq!(log.audio(0), vec![480, 480, -29_040, 480]);
    // A device running ahead of real time: the excess is trimmed at the front.
    rec.audio(1, &chunk(48_000), at(1_000));
    rec.audio(1, &chunk(480), at(1_010));
    rec.audio(1, &chunk(9_600), at(1_020)); // 200 ms claimed, only the last 10 ms are new
    assert_eq!(log.audio(1), vec![48_000, 480, 480]);
    rec.audio(1, &chunk(480), at(900)); // entirely behind what was written
    assert_eq!(log.audio(1).len(), 3, "a fully overlapping chunk writes nothing");
    let mic_written: i64 = log.audio(1).iter().sum();
    assert_eq!(mic_written, 48_960, "track 1 ends exactly at 1020 ms of real time");
    rec.stop();
}

#[test]
fn tick_pads_silent_tracks_and_times_out_an_armed_start() {
    let (mut rec, log, path) = recorder("tick");
    let t0 = Instant::now();
    rec.start(path.clone(), None, t0);
    assert!(rec.tick(t0 + Duration::from_secs(5)).is_empty());
    rec.video(0, true, pkt(1), false, t0 + Duration::from_secs(5));
    rec.tick(t0 + Duration::from_secs(6));
    // 1 s in, padded up to 250 ms behind real time on both tracks.
    assert_eq!(log.audio(0), vec![-(36_000)]);
    assert_eq!(log.audio(1), vec![-(36_000)]);
    rec.tick(t0 + Duration::from_millis(6_050)); // only 50 ms more: below tolerance
    assert_eq!(log.audio(0).len(), 1);
    rec.stop();

    let (mut rec, _log, path) = recorder("armtimeout");
    rec.start(path, Some(0), t0);
    let events = rec.tick(t0 + ARM_TIMEOUT);
    assert!(matches!(&events[..], [Event::Failed { kept: None, .. }]), "{events:?}");
    assert!(!rec.is_active());
}

#[test]
fn stop_or_start_in_the_wrong_phase_is_reported_not_fatal() {
    let (mut rec, _log, path) = recorder("phase");
    let t0 = Instant::now();
    assert!(matches!(&rec.stop()[..], [Event::Log(_)]));
    rec.start(path.clone(), Some(0), t0);
    assert!(matches!(&rec.start(path.clone(), Some(0), t0)[..], [Event::Log(_)]), "second start ignored");
    assert!(matches!(&rec.stop()[..], [Event::Failed { kept: None, .. }]), "stopped while armed");
    assert!(!naming::part_of(&path).exists());
}

#[test]
fn a_failing_sink_keeps_the_partial_recording() {
    let (_, log, path) = recorder("fail");
    let mut rec = Recorder::new(FakeFactory { log: log.clone(), fail_open: false, fail_video_after: Some(2) }, tracks());
    let t0 = Instant::now();
    rec.start(path.clone(), Some(0), t0);
    for i in 0..3u64 {
        assert!(rec.video(i * 16 * MS, i == 0, pkt(i as u8), false, t0).iter().all(|e| matches!(e, Event::Started { .. })));
    }
    let events = rec.video(48 * MS, false, pkt(9), false, t0);
    assert_eq!(events, vec![Event::Failed { reason: "disk full".into(), kept: Some(path.clone()) }]);
    assert!(path.is_file() && !naming::part_of(&path).exists(), "partial renamed into place");
    assert!(log.calls().contains(&Call::Abandon));
    assert!(!rec.is_active());

    let (_, log, path) = recorder("failopen");
    let mut rec = Recorder::new(FakeFactory { log, fail_open: true, fail_video_after: None }, tracks());
    rec.start(path.clone(), Some(0), t0);
    let events = rec.video(0, true, pkt(1), false, t0);
    assert_eq!(events, vec![Event::Failed { reason: "no encoder parameters".into(), kept: None }]);
    assert!(!path.exists() && !naming::part_of(&path).exists());
}

#[test]
fn marks_land_in_the_recording_sidecar_and_a_reopen_ends_it() {
    let (mut rec, _log, path) = recorder("marks");
    let t0 = Instant::now();
    rec.start(path.clone(), Some(0), t0);
    rec.mark(5 * MS); // before the first frame: ignored (armed)
    rec.video(100 * MS, true, pkt(1), false, t0);
    rec.mark(600 * MS);
    rec.video(1_100 * MS, false, pkt(2), false, t0);
    rec.mark(1_100 * MS);
    let events = rec.encoder_reopened();
    assert!(matches!(&events[0], Event::Log(_)));
    assert!(matches!(&events[1], Event::Stopped { marks: 2, .. }), "{events:?}");
    let sidecar = crate::bookmarks::sidecar_path(&path);
    assert_eq!(std::fs::read_to_string(sidecar).unwrap(), r#"{"version":1,"marks":[0.5,1.0]}"#);
    assert!(rec.encoder_reopened().is_empty(), "nothing to end when idle");
}
