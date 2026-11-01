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

/// The loading part of the editor: what the views draw.
#[derive(Default)]
pub struct Loading {
    /// `[start, end)` seconds not on the SSD yet.
    pub grey: Vec<(f64, f64)>,
    /// Share of the clip on the SSD.
    pub share: f64,
    /// Playback waits for the part ahead.
    pub buffering: bool,
    /// `started` of the open: runs the loading animation.
    pub since: Option<Instant>,
    seen_revision: u64,
    seen_wave: u64,
    wave_at: Option<Instant>,
    /// The engine's result for this open is in.
    pub prepared: bool,
    /// The generation that opened from the fill.
    pub generation: Option<u64>,
}

impl Loading {
    pub fn active(&self) -> bool {
        self.since.is_some()
    }
}

impl EditorState {
    /// The clip can play: `apply_prepared` follows when the copy is complete.
    pub fn apply_streaming(&mut self, generation: u64, clip: Box<StreamedClip>) {
        let clip = *clip;
        if generation != self.generation {
            clip.media.fill.cancel();
            if let Some(sound) = &clip.media.sound {
                sound.stop();
            }
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
    pub(super) fn tick_loading(&mut self) -> bool {
        if !self.loading.active() {
            return false;
        }
        let duration = self.duration();
        let Some(media) = self.preview.as_ref().and_then(|preview| preview.stream()) else {
            self.loading = Default::default();
            return true;
        };
        let mut changed = false;
        let revision = media.fill.revision();
        let outcome = media.fill.outcome();
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
        changed |= self.keep_buffer();
        if outcome == Outcome::Done && self.loading.prepared && self.loading.grey.is_empty() && !self.loading.buffering {
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
        let Some(preview) = self.preview.as_mut() else { return false };
        if self.loading.buffering {
            if !preview.stream_ready(pos, (pos + RESUME_MARGIN).min(end)) {
                return false;
            }
            self.loading.buffering = false;
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
        (!ready).then_some("Loading this part…")
    }
}
