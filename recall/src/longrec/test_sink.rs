//! Test doubles for the long recording: a [`Sink`] that logs every call and
//! holds its `.part` open the way `Fmp4Sink` does (no delete sharing, so a
//! rename while it is alive fails exactly as on the real file).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::machine::{Sink, SinkFactory};
use super::TrackInfo;

pub(crate) const MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Call {
    Open(PathBuf),
    Video { pts: u64, dur: u64, key: bool, tag: u8 },
    Audio { track: usize, frames: usize },
    Silence { track: usize, frames: u64 },
    Finish,
    Abandon,
}

/// Every call, plus each track's samples as the sink received them (silence as zeros).
#[derive(Clone, Default)]
pub(crate) struct Log(Arc<Mutex<Vec<Call>>>, Arc<Mutex<Vec<Vec<f32>>>>);

impl Log {
    fn push(&self, c: Call) {
        self.0.lock().unwrap().push(c);
    }
    fn keep(&self, track: usize, samples: impl IntoIterator<Item = f32>) {
        let mut pcm = self.1.lock().unwrap();
        if pcm.len() <= track {
            pcm.resize(track + 1, Vec::new());
        }
        pcm[track].extend(samples);
    }
    /// `track`'s interleaved stereo samples in the order the sink wrote them.
    pub(crate) fn pcm(&self, track: usize) -> Vec<f32> {
        self.1.lock().unwrap().get(track).cloned().unwrap_or_default()
    }
    pub(crate) fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().clone()
    }
    pub(crate) fn videos(&self) -> Vec<(u64, u64, bool, u8)> {
        self.calls().into_iter().filter_map(|c| match c {
            Call::Video { pts, dur, key, tag } => Some((pts, dur, key, tag)),
            _ => None,
        }).collect()
    }
    /// Frames per track in call order: positive = audio, negative = silence.
    pub(crate) fn audio(&self, track: usize) -> Vec<i64> {
        self.calls().into_iter().filter_map(|c| match c {
            Call::Audio { track: t, frames } if t == track => Some(frames as i64),
            Call::Silence { track: t, frames } if t == track => Some(-(frames as i64)),
            _ => None,
        }).collect()
    }
}

pub(crate) struct FakeSink {
    log: Log,
    part: PathBuf,
    /// Open like `Fmp4Sink`'s file: FILE_SHARE_READ only.
    _file: File,
    fail_video_after: Option<usize>,
    fail_finish: bool,
    videos: usize,
    no_pcm: bool,
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
        if !self.no_pcm {
            self.log.keep(track, samples.iter().copied());
        }
        Ok(())
    }
    fn silence(&mut self, track: usize, frames: u64) -> Result<(), String> {
        self.log.push(Call::Silence { track, frames });
        if !self.no_pcm {
            self.log.keep(track, std::iter::repeat_n(0.0, frames as usize * 2));
        }
        Ok(())
    }
    fn finish(&mut self) -> Result<u64, String> {
        self.log.push(Call::Finish);
        if self.fail_finish {
            return Err("write trailer: disk full".into());
        }
        Ok(std::fs::metadata(&self.part).map(|m| m.len()).unwrap_or(0))
    }
    fn abandon(&mut self) {
        self.log.push(Call::Abandon);
    }
}

#[derive(Default)]
pub(crate) struct FakeFactory {
    pub log: Log,
    pub fail_open: bool,
    pub fail_video_after: Option<usize>,
    pub fail_finish: bool,
    /// Log calls only, not the samples (a minutes-long run would hold hundreds of MB).
    pub no_pcm: bool,
}

impl SinkFactory for FakeFactory {
    type Sink = FakeSink;
    fn open(&mut self, part: &Path, _tracks: &[TrackInfo]) -> Result<FakeSink, String> {
        use std::os::windows::fs::OpenOptionsExt;
        if self.fail_open {
            return Err("no encoder parameters".into());
        }
        std::fs::write(part, b"fmp4").map_err(|e| e.to_string())?;
        let file = std::fs::OpenOptions::new().append(true).share_mode(0x1).open(part).map_err(|e| e.to_string())?;
        self.log.push(Call::Open(part.to_path_buf()));
        Ok(FakeSink {
            log: self.log.clone(),
            part: part.to_path_buf(),
            _file: file,
            fail_video_after: self.fail_video_after,
            fail_finish: self.fail_finish,
            videos: 0,
            no_pcm: self.no_pcm,
        })
    }
}

pub(crate) fn tracks() -> Vec<TrackInfo> {
    vec![TrackInfo { name: "Game", rate: 48_000, channels: 2, boosted: false }, TrackInfo { name: "Mic", rate: 48_000, channels: 2, boosted: false }]
}

pub(crate) fn pkt(tag: u8) -> Arc<[u8]> {
    Arc::from(vec![tag])
}

/// A fresh temp folder for one test.
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-longrec-{tag}-{}-{}", std::process::id(), uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
