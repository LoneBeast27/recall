//! Sound that fails while the picture works (no output device, a headset unplugged mid-play, a
//! stem that will not read) drops the clip to picture only instead of failing the preview: no
//! preview copy is made for it, the editor says "Sound unavailable", and the next Play tries
//! the sound again (WinMM's default device follows a newly plugged one).
use super::{NativePreview, PreviewTrack};
use crate::audio::stream::LiveTrack;

/// What the sound was opened from, kept to try again.
#[derive(Clone)]
pub(super) enum SoundSpec {
    Stems(Vec<PreviewTrack>, f64),
    Live(Vec<LiveTrack>, f64),
}

impl SoundSpec {
    /// (stream index, gain dB, muted) of every track.
    fn levels(&self) -> Vec<(usize, f64, bool)> {
        match self {
            SoundSpec::Stems(tracks, _) => tracks.iter().map(|t| (t.stream_index, t.gain_db, t.muted)).collect(),
            SoundSpec::Live(tracks, _) => tracks.iter().map(|t| (t.stream_index, t.gain_db, t.muted)).collect(),
        }
    }

    fn set_level(&mut self, stream_index: usize, gain_db: f64, muted: bool) {
        match self {
            SoundSpec::Stems(tracks, _) => tracks.iter_mut().filter(|t| t.stream_index == stream_index).for_each(|t| (t.gain_db, t.muted) = (gain_db, muted)),
            SoundSpec::Live(tracks, _) => tracks.iter_mut().filter(|t| t.stream_index == stream_index).for_each(|t| (t.gain_db, t.muted) = (gain_db, muted)),
        }
    }
}

/// Which part of the preview a failure belongs to.
#[derive(Debug, PartialEq)]
pub(crate) enum Failure {
    /// The preview fails (a preview copy may help).
    Picture(String),
    /// The clip goes on without sound.
    Sound(String),
}

/// A picture error fails the preview; a sound error alone does not.
pub(crate) fn classify(picture: Option<String>, sound: Option<String>) -> Option<Failure> {
    picture.map(Failure::Picture).or(sound.map(Failure::Sound))
}

/// What the editor shows about the sound.
#[derive(Debug, PartialEq)]
pub enum SoundNotice {
    Lost(String),
    Back,
}

#[derive(Default)]
pub(super) struct SoundFallback {
    spec: Option<SoundSpec>,
    /// Sound failed for this clip: the picture runs the clock.
    down: bool,
    /// A retry was sent and has not settled.
    retrying: bool,
    notice: Option<SoundNotice>,
}

impl SoundFallback {
    /// A clip opened with this sound (None: no sound, or the sound comes later).
    pub(super) fn opened(&mut self, spec: Option<SoundSpec>) {
        *self = Self { spec, ..Self::default() };
    }

    /// The sound failed: the clip goes on picture-only. A failed retry stays quiet (the editor
    /// already says so).
    pub(super) fn failed(&mut self, error: String) {
        if !self.down {
            self.notice = Some(SoundNotice::Lost(error));
        }
        self.down = true;
        self.retrying = false;
    }

    /// Play pressed: the sound to open again, once per press while it is down.
    pub(super) fn retry(&mut self) -> Option<SoundSpec> {
        let spec = self.spec.clone().filter(|_| self.down && !self.retrying)?;
        self.retrying = true;
        Some(spec)
    }

    pub(super) fn recovered(&mut self) {
        (self.down, self.retrying) = (false, false);
        self.notice = Some(SoundNotice::Back);
    }

    pub(super) fn is_down(&self) -> bool {
        self.down
    }

    pub(super) fn retrying(&self) -> bool {
        self.retrying
    }

    /// A level set while the sound is down still reaches it when it comes back.
    pub(super) fn set_level(&mut self, stream_index: usize, gain_db: f64, muted: bool) {
        if let Some(spec) = self.spec.as_mut() {
            spec.set_level(stream_index, gain_db, muted);
        }
    }

    /// The levels the sound should play at now.
    pub(super) fn levels(&self) -> Vec<(usize, f64, bool)> {
        self.spec.as_ref().map_or_else(Vec::new, SoundSpec::levels)
    }

    pub(super) fn take_notice(&mut self) -> Option<SoundNotice> {
        self.notice.take()
    }
}

impl NativePreview {
    /// Runs on every refresh and before Play: a sound failure drops the clip to picture only,
    /// and a retry that has loaded brings the sound back at the picture's time.
    pub(super) fn settle_sound(&mut self) {
        let sound_error = (self.has_audio || self.sound.retrying()).then(|| self.audio.error()).flatten();
        if let Some(Failure::Sound(error)) = classify(None, sound_error) {
            self.has_audio = false;
            let _ = self.audio.close();
            self.sound.failed(error);
            return;
        }
        if self.sound.retrying() && self.audio.is_ready() {
            let at = self.video_position().unwrap_or(0.0);
            let playing = self.is_playing();
            self.has_audio = true;
            self.sound.recovered();
            // Levels set while the retry loaded.
            for (stream_index, gain_db, muted) in self.sound.levels() {
                let _ = self.audio.set_audio(stream_index, gain_db, muted);
            }
            let _ = self.audio.seek(at);
            if playing {
                let _ = self.audio.play();
            }
        }
    }

    /// Play pressed while the sound is down: try it once more.
    pub(super) fn retry_sound(&mut self) {
        let Some(spec) = self.sound.retry() else { return };
        let sent = match spec {
            SoundSpec::Stems(tracks, duration) => self.audio.open(&tracks, duration),
            SoundSpec::Live(tracks, duration) => self.audio.open_live(tracks, duration),
        };
        if let Err(error) = sent {
            self.sound.failed(error);
        }
    }

    /// The sound went away or came back since the last call.
    pub fn take_sound_notice(&mut self) -> Option<SoundNotice> {
        self.sound.take_notice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sound_error_alone_does_not_fail_the_preview() {
        // E5: no output device (or an unplugged one) hid the picture and queued a full proxy.
        assert_eq!(classify(None, Some("could not open wave output (WinMM error 2)".into())), Some(Failure::Sound("could not open wave output (WinMM error 2)".into())));
        assert_eq!(classify(Some("bad video".into()), Some("no device".into())), Some(Failure::Picture("bad video".into())));
        assert_eq!(classify(None, None), None);
    }

    fn stems() -> SoundSpec {
        SoundSpec::Stems(vec![PreviewTrack { stream_index: 1, path: "a.flac".into(), gain_db: 0.0, muted: false }], 10.0)
    }

    #[test]
    fn a_lost_sound_is_said_once_retried_per_play_and_keeps_new_levels() {
        let mut sound = SoundFallback::default();
        sound.opened(Some(stems()));
        assert!(sound.retry().is_none(), "nothing to retry while the sound works");
        sound.failed("no device".into());
        assert_eq!(sound.take_notice(), Some(SoundNotice::Lost("no device".into())));
        sound.set_level(1, -6.0, true);
        let Some(SoundSpec::Stems(tracks, _)) = sound.retry() else { panic!("Play retries the stems") };
        assert_eq!((tracks[0].gain_db, tracks[0].muted), (-6.0, true), "the level set meanwhile");
        assert!(sound.retry().is_none(), "one retry at a time");
        sound.failed("still no device".into());
        assert_eq!(sound.take_notice(), None, "a failed retry stays quiet");
        assert!(sound.retry().is_some(), "the next Play tries again");
        sound.recovered();
        assert_eq!(sound.take_notice(), Some(SoundNotice::Back));
        assert!(!sound.is_down());
    }
}
