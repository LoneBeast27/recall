//! The encoder-thread half of a save: detach a self-contained `SaveJob` from
//! the live encoder (ring window or still source + PCM snapshots) and queue it
//! for the `recall-saver` thread.

use std::sync::Arc;
use std::time::Instant;

use super::saver::{self, ms, RawFrame, SaveJob, SaverQueue, Timing, VideoSrc};
use super::{Enc, EncStats, SaveRequest, StillSpec};
use crate::audio::TrackRing;

impl Enc {
    /// The newest frame's picture without canvas bars, for thumbnails.
    pub(super) fn thumb_of(&self, raw: &RawFrame) -> RawFrame {
        match self.last_content {
            Some(r) if r.clamp_to((raw.0, raw.1)) == Some(r) => {
                let (w, h, rgba) = crate::framing::crop_rgba(raw.0, &raw.2, r);
                (w, h, Arc::new(rgba))
            }
            _ => raw.clone(),
        }
    }

    /// The newest frame as RGBA for thumbnails / stills (a GPU frame is read back now).
    pub(super) fn raw_last(&self) -> Option<RawFrame> {
        if let Some(frame) = &self.last_frame {
            return Some(frame.clone());
        }
        let gpu = self.last_gpu.as_ref()?;
        match crate::gpu_frames::read_rgba(gpu) {
            Ok((w, h, rgba)) => Some((w, h, Arc::new(rgba))),
            Err(e) => {
                eprintln!("[recall] frame readback for the thumbnail failed: {e}");
                None
            }
        }
    }

    /// Detach everything a save needs from the live encoder: the ring window
    /// (refcounted payloads) or a still source, owned codec parameters, and
    /// each audio track's PCM snapshotted now, aligned to the video length.
    pub(super) fn build_job(
        &self,
        req: SaveRequest,
        audio: &[Arc<TrackRing>],
        started: Instant,
    ) -> Result<SaveJob, String> {
        let mut timing = Timing {
            request_wait_ms: ms(started.saturating_duration_since(req.requested)),
            ..Timing::default()
        };
        let phase = Instant::now();
        let end_us = req
            .end_us
            .unwrap_or_else(|| self.ring.last_pts_us().unwrap_or(0));
        let want_us = req.seconds as u64 * 1_000_000;
        let quiet_tail = self
            .last_input_pts
            .is_some_and(|pts| pts < end_us.saturating_sub(want_us));
        // Thumbnail + still source; a GPU frame is read back once, here.
        let raw = self.raw_last();
        let (video, secs) = if quiet_tail {
            // A fresh keyframe-only still: reusing an old inter-frame packet
            // would need its references and span the whole quiet gap. The still
            // lasts exactly the requested window (the ring holds it >= 1 µs).
            let frame = raw.clone().ok_or("no frame available for static save")?;
            let still = VideoSrc::Still {
                frame,
                spec: StillSpec {
                    fps: self.nominal_fps,
                    bitrate: self.bitrate,
                    codec: self.codec,
                    quality: self.quality,
                },
            };
            (still, want_us.max(1) as f64 / 1_000_000.0)
        } else {
            let packets = self.ring.save_window_at(want_us, end_us);
            if packets.is_empty() {
                return Err("replay buffer empty (no keyframe yet)".into());
            }
            let secs = saver::window_secs(&packets);
            let params = self.params.clone();
            (VideoSrc::Window { packets, params }, secs)
        };
        timing.window_ms = ms(phase.elapsed());
        let phase = Instant::now();
        let audio = audio.iter().map(|ring| ring.snapshot_pcm(secs)).collect();
        timing.snapshot_ms = ms(phase.elapsed());
        Ok(SaveJob {
            path: req.path,
            seconds: req.seconds,
            requested: req.requested,
            secs,
            video,
            backend: self.backend,
            audio,
            // The still keeps the whole canvas frame; the thumbnail is the picture only.
            thumb: raw.as_ref().map(|r| self.thumb_of(r)),
            width: self.width,
            height: self.height,
            fps: self.nominal_fps,
            session_id: self.session_id.clone(),
            timing,
        })
    }
}

/// Encoder-thread half of a save: build the job and queue it. Never muxes;
/// a busy writer fails the save immediately instead of stalling capture.
pub(super) fn handle_save(
    enc: Option<&Enc>,
    req: SaveRequest,
    stats: &EncStats,
    audio: &[Arc<TrackRing>],
    saver: &SaverQueue,
) {
    let started = Instant::now();
    let Some(enc) = enc else {
        let reason = stats
            .error
            .lock()
            .ok()
            .and_then(|e| e.clone())
            .unwrap_or_else(|| "encoder not initialized (no frames yet)".into());
        saver.events().save_failed(&reason);
        return;
    };
    match enc.build_job(req, audio, started) {
        Ok(mut job) => {
            job.timing.encoder_blocked_ms = ms(started.elapsed());
            saver.submit(job);
        }
        Err(reason) => saver.events().save_failed(&reason),
    }
}
