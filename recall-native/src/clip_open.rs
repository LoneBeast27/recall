//! Opening a clip without waiting for the slow drive. A clip on a hard drive starts copying to
//! the SSD, the player and the sound decoder read that copy as it grows, and the editor gets
//! the clip as soon as the file's index is in. The engine's stems, waveforms and filmstrip
//! follow when the copy is complete (`Done::Prepared` then upgrades the open clip).
//! A clip too large for the video tier takes the same path without the copy (`ClipFill::direct`).
//! Anything that cannot stream (no sample tables, no ffprobe) opens the old way.
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use crate::{
    audio::stream as live_sound,
    backend::{self, Project, VisualAssets},
    edit_model::{AudioTrackInfo, ClipInfo},
    media::{SourceHandle, StreamMedia, open_source},
    services::{
        library::mp4_map::{self, ClipMap},
        storage::{
            self, Priority,
            fill::{AlignedBuffer, CHUNK, ClipFill},
        },
    },
    worker::Done,
};

/// A clip ready to play while it loads.
pub struct StreamedClip {
    pub info: ClipInfo,
    pub project: Option<Project>,
    pub warning: Option<String>,
    /// As [`backend::PreparedClip`]: this session may touch the sidecar; one was there at open.
    pub sidecar_ours: bool,
    pub sidecar_at_open: bool,
    pub visuals: VisualAssets,
    pub marks: Vec<f64>,
    pub media: StreamMedia,
}

/// The newest open: an older one stops where it is, its engine included.
static LATEST: AtomicU64 = AtomicU64::new(0);

/// Called on the UI thread as an open starts, so a newer clip is known before its thread runs.
pub fn note_open(generation: u64) {
    LATEST.store(generation, Ordering::Release);
}

fn stale(generation: u64) -> bool {
    LATEST.load(Ordering::Acquire) != generation
}

/// Orders the keep marks of opens that overlap, so an older open never marks over a newer one.
static KEEP_MARK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Marks the clip as the one in the editor before anything resolves its SSD copy: the cache
/// keeps that copy whole from here on. Marked only at the end of preparing, the copy was fair
/// game for the whole engine run, and a list reload or the recorder's kept copy deleted it
/// under the player (Media Foundation 0x80070002, no filmstrip). False: a newer open started.
fn keep_while_opening(generation: u64, path: &str) -> bool {
    let _order = KEEP_MARK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if stale(generation) {
        return false;
    }
    storage::note_editor_media(path, &[], true);
    true
}

/// Runs one open from start to finish on its own thread: exports and listings keep their queue.
pub fn run(generation: u64, path: String, tx: &Sender<Done>) {
    if !keep_while_opening(generation, &path) {
        return;
    }
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    match stream(generation, &path) {
        Some(clip) => {
            let fill = clip.media.fill.clone();
            let _ = tx.send(Done::Streaming(generation, Box::new(clip)));
            finish(generation, &path, &fill, tx);
        }
        None if stale(generation) => {}
        None => {
            let step = |text: &'static str| {
                let _ = tx.send(Done::OpenStep(generation, text));
            };
            // A clip in the SSD video tier is prepared from that copy straight away: it does not
            // queue on the hard drive's lane, and nothing waits for the recorder.
            let copy = storage::cached_video(&path);
            let _ = tx.send(Done::Prepared(generation, backend::prepare_clip_steps(&path, copy, false, &step, Arc::new(move || stale(generation)))));
        }
    }
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
}

/// The copy is complete: the engine prepares stems, waveforms and filmstrip from it. A clip
/// read where it lies is prepared where it lies, at very low I/O priority, around the playback.
/// Neither waits for the recorder or for the editor to be on screen: an open always finishes.
fn finish(generation: u64, path: &str, fill: &ClipFill, tx: &Sender<Done>) {
    if !fill.wait_done() {
        if let Some(error) = fill.error() {
            let _ = tx.send(Done::Prepared(generation, Err(error)));
        }
        return;
    }
    let direct = fill.is_direct();
    let copy = (!direct).then(|| fill.target().to_string_lossy().into_owned());
    let result = backend::prepare_clip_steps(path, copy, direct, &|_| {}, Arc::new(move || stale(generation)));
    let _ = tx.send(Done::Prepared(generation, result));
}

fn stream(generation: u64, path: &str) -> Option<StreamedClip> {
    let source = std::fs::canonicalize(path).ok()?;
    let metadata = source.metadata().ok().filter(|m| m.is_file())?;
    let streamable = matches!(source.extension()?.to_str()?.to_ascii_lowercase().as_str(), "mp4" | "mov" | "m4v");
    if !streamable || !storage::is_slow(&source) || storage::cached_video(&source.to_string_lossy()).is_some() {
        return None;
    }
    // A large clip prepared before: the engine's entry is one read away.
    if storage::too_large(metadata.len()) && backend::has_prepared(&source, &metadata) {
        return None;
    }
    if stale(generation) {
        return None;
    }
    match open_streaming(&source, &metadata) {
        Ok(clip) => Some(clip),
        Err(error) => {
            crate::trace::player_event("clip opens the old way", serde_json::json!({ "clip": path, "why": error }));
            None
        }
    }
}

fn open_streaming(source: &Path, metadata: &std::fs::Metadata) -> Result<StreamedClip, String> {
    let lane_source = source.to_path_buf();
    let (probed, moov) = storage::run_io(source, Priority::Interactive, move || -> Result<_, String> {
        let probed = storage::probe_clip(&lane_source, storage::drive_kind(&lane_source).is_hdd())?;
        let mut file = File::open(&lane_source).map_err(|error| format!("open clip: {error}"))?;
        let moov = mp4_map::moov_range(&mut file).ok_or("clip has no index")?;
        Ok((probed, moov))
    })??;
    let index_chunks = moov.0 / CHUNK..=(moov.1.saturating_sub(1)) / CHUNK;
    let first: Vec<u64> = index_chunks.chain(0..1).collect();
    let fill = storage::start_fill(source, metadata, &first)?;
    let opened = open_fill(source, metadata, probed, &fill);
    if opened.is_err() {
        fill.cancel();
    }
    opened
}

fn open_fill(source: &Path, metadata: &std::fs::Metadata, probed: storage::ProbedClip, fill: &ClipFill) -> Result<StreamedClip, String> {
    let mut reader = FillReader::new(fill)?;
    let map = Arc::new(mp4_map::clip_map(&mut reader).ok_or("clip has no sample tables")?);
    // The light copy, when the clip has one, shows the picture at once; the fill brings the sound
    // and, once it is done, the full-resolution picture (`EditorState::tick_loading`).
    let light_source = storage::light_copy(&source.to_string_lossy()).and_then(|path| crate::media::open_file_source(&path).ok());
    let light = light_source.is_some();
    let video = SourceHandle(match light_source {
        Some(source) => source,
        None => open_source(fill, true)?,
    });
    let info = clip_info(source, metadata, &probed, &map);
    let (project, warning, sidecar_ours, sidecar_at_open) = backend::load_project(&info);
    let tracks: Vec<(usize, u16)> = probed.tracks.iter().map(|track| (track.stream_index, track.channels)).collect();
    let sound = (!tracks.is_empty()).then(|| start_sound(fill, &map, &tracks, &probed)).flatten();
    let plain = source.to_string_lossy();
    storage::note_editor_media(&plain, &[], true);
    let marks = backend::read_marks(&plain);
    let visuals = backend::cached_visuals(source);
    Ok(StreamedClip { info, project, warning, sidecar_ours, sidecar_at_open, visuals, marks, media: StreamMedia { fill: fill.clone(), map, video, sound, light } })
}

fn start_sound(fill: &ClipFill, map: &Arc<ClipMap>, tracks: &[(usize, u16)], probed: &storage::ProbedClip) -> Option<Arc<live_sound::LiveSound>> {
    let started = open_source(fill, false).and_then(|source| live_sound::start(source, fill.clone(), Arc::clone(map), tracks, probed.origin, probed.duration));
    match started {
        Ok(sound) => Some(sound),
        Err(error) => {
            crate::trace::player_event("sound waits for the stems", serde_json::json!({ "why": error }));
            None
        }
    }
}

/// The engine's `ClipInfo`, from the file's index. Stems and waveforms come later.
fn clip_info(source: &Path, metadata: &std::fs::Metadata, probed: &storage::ProbedClip, map: &ClipMap) -> ClipInfo {
    let audio = probed
        .tracks
        .iter()
        .map(|track| AudioTrackInfo {
            stream_index: track.stream_index,
            name: track.name.clone().unwrap_or_else(|| format!("Audio {}", track.stream_index + 1)),
            channels: track.channels,
            sample_rate: 48_000,
            preview_path: String::new(),
            waveform: Vec::new(),
            source: track.source.clone(),
        })
        .collect();
    let mut info = ClipInfo {
        path: source.to_string_lossy().into_owned(),
        duration: probed.duration,
        width: probed.width,
        height: probed.height,
        video_fps: probed.fps,
        source_bytes: metadata.len(),
        source_modified_ms: backend::modified_ms(metadata),
        audio,
        keyframes: map.keyframes.iter().map(|seconds| (seconds - probed.origin).max(0.0)).collect(),
        ..Default::default()
    };
    backend::normalize_audio_names(&mut info);
    info
}

/// The fill as a plain file for the index parser: reads wait for their bytes.
struct FillReader<'a> {
    fill: &'a ClipFill,
    file: File,
    scratch: AlignedBuffer,
    position: u64,
}

impl<'a> FillReader<'a> {
    fn new(fill: &'a ClipFill) -> Result<Self, String> {
        let file = fill.open_reader().map_err(|error| format!("open clip fill: {error}"))?;
        Ok(Self { fill, file, scratch: AlignedBuffer::new(1 << 20), position: 0 })
    }
}

impl Read for FillReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let length = (out.len() as u64).min(self.fill.bytes().saturating_sub(self.position)).min((1 << 20) - 8192);
        if length == 0 {
            return Ok(0);
        }
        if !self.fill.wait_for(self.position, self.position + length, true) {
            return Err(io::Error::other("clip closed"));
        }
        let read = self.fill.read_at(&self.file, self.position, &mut out[..length as usize], &mut self.scratch)?;
        self.position += read as u64;
        Ok(read)
    }
}

impl Seek for FillReader<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.position = match to {
            SeekFrom::Start(at) => at,
            SeekFrom::End(back) => self.fill.bytes().saturating_add_signed(back),
            SeekFrom::Current(step) => self.position.saturating_add_signed(step),
        };
        Ok(self.position)
    }
}

