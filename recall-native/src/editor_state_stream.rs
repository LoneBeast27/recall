//! Editor side of a clip that is still loading: which parts of the timeline are grey, the live
//! waveform, pausing for the part that is not there yet, and the upgrade when the engine has
//! prepared the stems and filmstrip.
use std::time::{Duration, Instant};

use super::EditorState;
use crate::{backend::PreparedClip, clip_open::StreamedClip, services::storage::fill::Outcome};

/// Playback stops this far before the loaded part ends...
const STOP_MARGIN: f64 = 0.4;
/// ...and restarts with this much ready.
const RESUME_MARGIN: f64 = 1.5;
const WAVE_EVERY: Duration = Duration::from_millis(120);
/// The picture counts as there this close to the playhead...
const PICTURE_NEAR: f64 = 0.15;
/// ...and playback stops waiting for it after this long.
const PICTURE_WAIT: Duration = Duration::from_secs(3);

/// What playback does once the rest of the clip will not come.
#[derive(Debug, PartialEq)]
enum AfterFailure {
    Carry,
    Resume,
    Stop,
}

/// `waiting`: it was buffering. `ready`: the part ahead did load.
fn after_failure(waiting: bool, ready: bool) -> AfterFailure {
    match (waiting, ready) {
        (true, true) => AfterFailure::Resume,
        (false, true) => AfterFailure::Carry,
        (_, false) => AfterFailure::Stop,
    }
}

/// The loading part of the editor: what the views draw.
#[derive(Default)]
pub struct Loading {
    /// `[start, end)` seconds not on the SSD yet.
    pub grey: Vec<(f64, f64)>,
    /// Share of the clip on the SSD.
    pub share: f64,
    /// Playback waits for the part ahead.
    pub buffering: bool,
    /// When the bytes for the buffered part arrived.
    ready_at: Option<Instant>,
    /// `started` of the open: runs the loading animation.
    pub since: Option<Instant>,
    seen_revision: u64,
    seen_wave: u64,
    wave_at: Option<Instant>,
    /// The engine's result for this open is in.
    pub prepared: bool,
    /// The generation that opened from the fill.
    pub generation: Option<u64>,
    /// Why the rest of the clip will not arrive.
    pub failed: Option<String>,
    paced: bool,
}

impl Loading {
    pub fn active(&self) -> bool {
        self.since.is_some()
    }
}

/// A clip read where it lies keeps its decoder: the stems are not swapped in while it is open.
fn media_ended_sound(preview: Option<&crate::media::NativePreview>) -> bool {
    preview.and_then(|preview| preview.stream()).is_none_or(|media| !media.fill.is_direct())
}

impl EditorState {
    /// The clip can play: `apply_prepared` follows when the copy is complete.
    pub fn apply_streaming(&mut self, generation: u64, clip: Box<StreamedClip>) {
        let clip = *clip;
        if generation != self.generation {
            clip.media.shut();
            return;
        }
        self.loading_path = None;
        self.sidecar_ours = clip.sidecar_ours;
        self.sidecar_at_open = clip.sidecar_at_open;
        self.adopt(clip.info.clone(), clip.project, clip.warning);
        let info = clip.info;
        let tracks = self.preview_tracks(&info);
        self.playback_path = Some(clip.media.fill.target().to_string_lossy().into_owned());
        self.preview_failed = false;
        let opened = match self.preview.as_mut() {
            Some(preview) => preview.open_stream(clip.media, &tracks, info.duration),
            None => Ok(()),
        };
        if let Err(error) = opened {
            self.status = error;
        }
        self.loading = super::stream::Loading { since: Some(Instant::now()), generation: Some(generation), grey: vec![(0.0, info.duration)], ..Default::default() };
        self.marks = clip.marks;
        self.info = Some(info);
        self.visuals = clip.visuals;
        self.applied_rect = (0, 0, 0, 0);
    }

    /// The engine finished a clip already playing from its fill: keep the plan, the player and
    /// the position, and take the prepared media.
    pub(super) fn upgrade(&mut self, prepared: PreparedClip) {
        self.loading.prepared = true;
        let Some(info) = self.info.as_mut() else { return };
        for (now, track) in info.audio.iter_mut().zip(prepared.info.audio) {
            now.preview_path = track.preview_path;
            now.waveform = track.waveform;
            now.name = track.name;
        }
        info.keyframes = prepared.info.keyframes;
        self.visuals = prepared.visuals;
        if let Some(warning) = prepared.warning {
            self.warning = Some(warning);
        }
        let info = self.info.clone();
        if let Some(info) = info.filter(|_| self.preview.as_ref().is_some_and(|preview| preview.sound_waits_for_stems())) {
            let at = self.position();
            let tracks = self.preview_tracks(&info);
            if let Some(preview) = self.preview.as_mut() {
                match preview.use_stems(&tracks, info.duration) {
                    Ok(()) => self.pending_seek = Some(at),
                    Err(error) => self.last_error = Some(error),
                }
            }
        }
    }

    /// Per-poll upkeep while the clip loads. True when something on screen changed.
    pub(super) fn tick_loading(&mut self, editor_visible: bool) -> bool {
        if !self.loading.active() {
            return false;
        }
        let duration = self.duration();
        let Some(media) = self.preview.as_ref().and_then(|preview| preview.stream()) else {
            self.loading = Default::default();
            return true;
        };
        if hand_over_due(media.light, media.fill.outcome() == Outcome::Done, self.loading.prepared, self.playing, self.drag.is_some()) {
            let full = media.fill.target().to_string_lossy().into_owned();
            if let Some(clip) = self.info.as_ref().map(|info| info.path.clone()) {
                crate::trace::player_event("full resolution takes over from the light copy", serde_json::json!({ "at": self.position() }));
                self.on_video_cached(&clip, &full);
            }
            return true;
        }
        let mut changed = false;
        // Nobody is watching, or the recorder needs the drive: the copy slows down, never stops.
        let paced = !editor_visible || crate::services::storage::recorder_holds(media.fill.source());
        if paced != self.loading.paced {
            self.loading.paced = paced;
            media.fill.pace(paced);
        }
        let revision = media.fill.revision();
        let outcome = media.fill.outcome();
        if media.fill.is_direct() {
            // Nothing to copy: what is not ready yet is the sound, until the stems are in.
            let spans = if self.loading.prepared { Vec::new() } else { media.sound.as_ref().map_or_else(Vec::new, |sound| sound.missing_spans()) };
            if spans != self.loading.grey {
                let left: f64 = spans.iter().map(|span| span.1 - span.0).sum();
                self.loading.share = 1.0 - (left / duration).clamp(0.0, 1.0);
                self.loading.grey = spans;
                changed = true;
            }
        } else {
            if revision != self.loading.seen_revision {
                self.loading.seen_revision = revision;
                self.loading.share = media.fill.progress();
                self.loading.grey = media.map.missing_spans(&media.fill.holes(), duration);
                changed = true;
            }
            if outcome == Outcome::Done && !self.loading.grey.is_empty() {
                self.loading.grey.clear();
                changed = true;
            }
        }
        let wave_due = self.loading.wave_at.is_none_or(|at| at.elapsed() >= WAVE_EVERY);
        if wave_due && !self.loading.prepared {
            let peaks: Option<Vec<(usize, Vec<f32>)>> = media.sound.as_ref().and_then(|sound| {
                let total: u64 = sound.tracks.iter().map(|(_, pcm)| pcm.revision()).sum();
                (total != self.loading.seen_wave).then(|| {
                    self.loading.seen_wave = total;
                    sound.tracks.iter().map(|(index, pcm)| (*index, pcm.peaks())).collect()
                })
            });
            self.loading.wave_at = Some(Instant::now());
            if let (Some(peaks), Some(info)) = (peaks, self.info.as_mut()) {
                for (index, wave) in peaks {
                    if let Some(track) = info.audio.iter_mut().find(|track| track.stream_index == index) {
                        track.waveform = wave;
                    }
                }
                changed = true;
            }
        }
        if outcome == Outcome::Failed && self.loading.failed.is_none() {
            let why = media.fill.error().unwrap_or_else(|| "The clip's drive stopped answering.".into());
            crate::trace::player_event("clip stopped loading", serde_json::json!({ "why": why }));
            self.loading.failed = Some(why);
            changed = true;
        }
        changed |= self.keep_buffer();
        // Loading ends with the light copy gone: full resolution takes over first (above).
        let light = self.preview.as_ref().is_some_and(|preview| preview.shows_light_copy());
        if outcome == Outcome::Done && self.loading.prepared && self.loading.grey.is_empty() && !self.loading.buffering && !light && media_ended_sound(self.preview.as_ref()) {
            self.loading = Default::default();
            changed = true;
        }
        changed
    }

    /// Pauses before the playhead runs out of loaded clip, and carries on when there is enough.
    fn keep_buffer(&mut self) -> bool {
        if !self.playing || self.play_when_ready {
            return false;
        }
        let pos = self.position();
        let end = self.duration();
        let failed = self.loading.failed.is_some();
        let Some(preview) = self.preview.as_mut() else { return false };
        // What did not load will not come: playback ends where the loaded part ends.
        if failed {
            let ready = preview.stream_ready(pos, (pos + STOP_MARGIN).min(end));
            let waiting = std::mem::take(&mut self.loading.buffering);
            self.loading.ready_at = None;
            match after_failure(waiting, ready) {
                AfterFailure::Carry => return false,
                AfterFailure::Resume => self.play(),
                AfterFailure::Stop => self.pause(),
            }
            return true;
        }
        if self.loading.buffering {
            // A pause that beat a play still on its way to the player did nothing.
            if preview.video_state() == Some("playing") {
                let _ = preview.pause();
            }
            if !preview.stream_ready(pos, (pos + RESUME_MARGIN).min(end)) {
                self.loading.ready_at = None;
                return false;
            }
            // A seek past the copy lands only now: starting the sound first leaves the picture behind.
            let waited = *self.loading.ready_at.get_or_insert_with(Instant::now);
            let near = preview.video_position().is_ok_and(|video| (video - pos).abs() < PICTURE_NEAR);
            if !near && waited.elapsed() < PICTURE_WAIT {
                return false;
            }
            self.loading.buffering = false;
            self.loading.ready_at = None;
            self.play();
            return true;
        }
        if preview.stream_ready(pos, (pos + STOP_MARGIN).min(end)) {
            return false;
        }
        self.loading.buffering = true;
        let _ = preview.pause();
        self.watch.reset();
        true
    }

    /// What the preview says while its picture cannot be shown.
    pub fn loading_note(&self) -> Option<&'static str> {
        if !self.loading.active() {
            return None;
        }
        if self.loading.buffering {
            return Some("Buffering…");
        }
        let pos = self.position();
        let ready = self.preview.as_ref().is_none_or(|preview| preview.stream_ready(pos, (pos + 0.05).min(self.duration())));
        if ready {
            return None;
        }
        Some(if self.loading.failed.is_some() { "This part didn't load" } else { "Loading this part…" })
    }
}

/// Full resolution replaces the light copy once the whole clip is on the SSD and its stems are
/// in (the reopened player plays from them), at a moment playback is stopped and no timeline
/// drag (scrub, trim, gain) is under way: a pause, the end, or before the first play. Swapping
/// mid-play would show a hitch; mid-drag it would drop the scrub's frames and position.
fn hand_over_due(light: bool, copied: bool, prepared: bool, playing: bool, dragging: bool) -> bool {
    light && copied && prepared && !playing && !dragging
}

impl EditorState {
    /// The picture on screen is the light copy: the transport shows "Preview quality".
    pub fn preview_quality(&self) -> bool {
        self.preview.as_ref().is_some_and(|preview| preview.shows_light_copy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_resolution_takes_over_only_when_whole_prepared_and_stopped() {
        assert!(hand_over_due(true, true, true, false, false), "copied, stems in, paused: swap");
        assert!(!hand_over_due(true, true, true, true, false), "never mid-play");
        assert!(!hand_over_due(true, true, true, false, true), "never mid-drag, even paused: it waits for the drag to end");
        assert!(!hand_over_due(true, false, true, false, false), "the whole clip is not on the SSD yet");
        assert!(!hand_over_due(true, true, false, false, false), "the stems are not in yet: the sound comes from the fill");
        assert!(!hand_over_due(false, true, true, false, false), "a fill without a light copy already shows full resolution");
    }

    #[test]
    fn a_failure_always_ends_a_wait() {
        assert_eq!(after_failure(true, true), AfterFailure::Resume, "what is left of the loaded part plays");
        assert_eq!(after_failure(true, false), AfterFailure::Stop);
        assert_eq!(after_failure(false, true), AfterFailure::Carry);
        assert_eq!(after_failure(false, false), AfterFailure::Stop);
    }
}
