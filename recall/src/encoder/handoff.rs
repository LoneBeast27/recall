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
        let request_end_us = req
            .end_us
            .unwrap_or_else(|| self.ring.last_pts_us().unwrap_or(0));
        let end_us = video_end_us(request_end_us, self.ring.end_us(), req.at_last_frame);
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
        // The audio rings' newest sample is "now". On the capture clock that is the
        // request's end plus the wait since; the clip's video ends at `end_us`.
        let tail_secs = match req.end_us {
            Some(at_request) => audio_tail_us(at_request, req.requested.elapsed(), end_us) as f64 / 1_000_000.0,
            None => 0.0, // no capture clock (smoke paths): the newest audio, as before
        };
        let audio = audio.iter().map(|ring| ring.snapshot_pcm_ending(secs, tail_secs)).collect();
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

/// Where a save's video ends on the capture clock: the newest frame held up to
/// the request (a static scene keeps its picture and its sound), or, once the
/// game has closed (`at_last_frame`), the end of its last frame.
pub(crate) fn video_end_us(request_end_us: u64, last_frame_end_us: Option<u64>, at_last_frame: bool) -> u64 {
    match last_frame_end_us {
        Some(last) if at_last_frame => last,
        Some(last) => request_end_us.max(last),
        None => request_end_us,
    }
}

/// Audio recorded after the clip's video ends: the rings' newest sample is
/// `request_end_us + waited` on the capture clock (the snapshot happens after
/// the request), so this much is cut off their end. Audio then spans exactly
/// [video_end - secs, video_end], the same window as the video.
pub(crate) fn audio_tail_us(request_end_us: u64, waited: std::time::Duration, video_end_us: u64) -> u64 {
    request_end_us.saturating_add(waited.as_micros() as u64).saturating_sub(video_end_us)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_video_end_and_the_audio_cut_share_the_capture_clock() {
        // Live: the newest frame (60 s) is held to the request (61 s); the audio
        // snapshot 3 ms later drops those 3 ms.
        assert_eq!(video_end_us(61_000_000, Some(60_000_000), false), 61_000_000);
        assert_eq!(audio_tail_us(61_000_000, Duration::from_millis(3), 61_000_000), 3_000);
        // The game closed at 60 s; saved 30 s into the drain: the clip ends at the
        // last frame and the 30 s of audio after it are left out.
        assert_eq!(video_end_us(90_000_000, Some(60_000_000), true), 60_000_000);
        assert_eq!(audio_tail_us(90_000_000, Duration::from_millis(3), 60_000_000), 30_003_000);
        // An empty ring keeps the request's end.
        assert_eq!(video_end_us(5, None, true), 5);
    }
}
