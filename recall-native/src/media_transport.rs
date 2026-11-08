//! `NativePreview` transport (play/pause/seek, A/V drift correction), placement of the
//! video window, and diagnostics.
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::RECT;
use windows::Win32::Media::MediaFoundation::MFP_MEDIAPLAYER_STATE_PLAYING;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetParent, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow,
};

use super::{NativePreview, player_position, player_state_name, rect_json, set_player_position, video_time};

impl NativePreview {
    pub fn diagnostics(&self) -> serde_json::Value {
        let mut client = RECT::default();
        let client_error = unsafe { GetClientRect(self.window, &mut client) }
            .err()
            .map(|error| error.to_string());
        let mut screen = RECT::default();
        let screen_error = unsafe { GetWindowRect(self.window, &mut screen) }
            .err()
            .map(|error| error.to_string());
        let parent = unsafe { GetParent(self.window) }.ok();
        let mut process_id = 0u32;
        let thread_id = unsafe { GetWindowThreadProcessId(self.window, Some(&mut process_id)) };
        let player_state = self
            .player
            .as_ref()
            .and_then(|player| unsafe { player.GetState() }.ok());
        let video_position = self.player.as_ref().map(|player| player_position(player));
        let paint_error = self
            .window_state
            .update_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        serde_json::json!({
            "window": {
                "hwnd": self.window.0 as usize,
                "parent_hwnd": parent.map(|hwnd| hwnd.0 as usize),
                "client_bounds": rect_json(client),
                "client_bounds_error": client_error,
                "screen_bounds": rect_json(screen),
                "screen_bounds_error": screen_error,
                "visible": unsafe { IsWindowVisible(self.window).as_bool() },
                "thread_id": thread_id,
                "process_id": process_id,
                "paint_count": self.window_state.paint_count.load(Ordering::Relaxed),
                "video_update_count": self.window_state.update_count.load(Ordering::Relaxed),
                "video_update_error": paint_error,
                "video_presentable": self.window_state.presentable.load(Ordering::Acquire),
            },
            "ready": self.is_ready(),
            "media_ready": self.media_status.ready.load(Ordering::Acquire),
            "audio_ready": !self.has_audio || self.audio.is_ready(),
            "error": self.error(),
            "player_state": player_state.map(player_state_name),
            "player_state_code": player_state.map(|state| state.0),
            "priming_first_frame": self.prime_started.is_some(),
            "video_position_seconds": video_position.as_ref().and_then(|value| value.as_ref().ok()).copied(),
            "video_position_error": video_position.and_then(Result::err),
            "audio_position_seconds": self.has_audio.then(|| self.audio.position()),
            "audio_playing": self.has_audio && self.audio.is_playing(),
            "audio_underruns": self.audio_underruns(),
        })
    }

    pub fn error(&self) -> Option<String> {
        self.media_status
            .error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .or_else(|| {
                self.window_state
                    .update_error
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
            })
    }

    pub fn is_playing(&self) -> bool {
        if self.has_audio {
            self.audio.is_playing()
        } else {
            self.player
                .as_ref()
                .and_then(|player| unsafe { player.GetState() }.ok())
                .is_some_and(|state| state == MFP_MEDIAPLAYER_STATE_PLAYING)
        }
    }

    pub fn rect(&mut self, x: i32, y: i32, width: i32, height: i32) -> Result<(), String> {
        if width == 0 || height == 0 {
            return self.hide();
        }
        if width < 0 || height < 0 {
            return Err("preview rectangle dimensions cannot be negative".into());
        }
        unsafe { ShowWindow(self.window, SW_SHOWNOACTIVATE) };
        unsafe {
            SetWindowPos(
                self.window,
                None,
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        }
        .map_err(|error| format!("could not resize preview window: {error}"))?;
        Ok(())
    }

    pub fn hide(&mut self) -> Result<(), String> {
        unsafe { ShowWindow(self.window, SW_HIDE) };
        Ok(())
    }

    pub fn play(&mut self) -> Result<(), String> {
        self.settle_sound();
        self.retry_sound();
        self.require_ready()?;
        self.prime_started = None;
        self.rest.set(None);
        if self.has_audio {
            self.place_video(self.audio.position())?;
        }
        let player = self.player.as_ref().ok_or("preview video is not open")?;
        unsafe { player.Play() }
            .map_err(|error| format!("could not play native video: {error}"))?;
        if self.has_audio {
            if let Err(error) = self.audio.play() {
                let _ = unsafe { player.Pause() };
                return Err(error);
            }
        }
        self.last_drift_check.set(Instant::now());
        self.audio_seen.set((self.audio.position(), Instant::now()));
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), String> {
        self.prime_started = None;
        if let Some(player) = &self.player {
            if self.has_audio {
                self.audio.pause()?;
            }
            // At end of stream MFPlay stops by itself; pausing a stopped session fails
            // with MF_E_SESSION_PAUSEWHILESTOPPED, so only pause a playing player.
            let state = unsafe { player.GetState() }.unwrap_or(MFP_MEDIAPLAYER_STATE_PLAYING);
            if state == MFP_MEDIAPLAYER_STATE_PLAYING {
                unsafe { player.Pause() }
                    .map_err(|error| format!("could not pause native video: {error}"))?;
            }
        }
        if self.has_audio && self.player.is_some() {
            self.place_video(self.audio.position())?;
        }
        Ok(())
    }

    /// The clip has sound stems (the sound runs the clock); without, the picture does.
    pub fn has_sound(&self) -> bool {
        self.has_audio
    }

    /// The latest time the clock can reach: the clip (the sound is padded to its length), or
    /// the end of the video track for a clip without sound. None until known.
    pub fn clock_end(&self) -> Option<f64> {
        if self.has_audio {
            let end = self.audio.duration();
            (end > 0.0).then_some(end)
        } else {
            self.video_end
        }
    }

    /// The latest time the picture can show (the last frame stays up from here to the end).
    pub fn picture_end(&self) -> Option<f64> {
        self.last_frame.map(|last| video_time(last, self.last_frame))
    }

    /// Moves the picture to `seconds`, never onto the video track's last frame.
    fn place_video(&self, seconds: f64) -> Result<(), String> {
        let player = self.player.as_ref().ok_or("preview video is not open")?;
        set_player_position(player, video_time(seconds, self.last_frame))
    }

    /// The sound runs past the picture's last frame: nothing for the picture to follow.
    fn past_video_end(&self, seconds: f64) -> bool {
        self.picture_end().is_some_and(|end| seconds >= end)
    }

    pub fn seek(&mut self, seconds: f64) -> Result<(), String> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err("preview seek must be a finite non-negative time".into());
        }
        self.require_ready()?;
        self.prime_started = None;
        let player = self.player.as_ref().ok_or("preview video is not open")?;
        if self.has_audio {
            self.audio.seek(seconds)?;
        }
        let picture = video_time(seconds, self.last_frame);
        set_player_position(player, picture)?;
        if let Some(media) = &self.stream {
            media.fill.note_seek();
        }
        self.rest.set((picture < seconds).then_some(seconds));
        self.last_drift_check.set(Instant::now());
        self.audio_seen.set((seconds, Instant::now()));
        Ok(())
    }

    pub fn position(&self) -> Result<f64, String> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        if !self.has_audio {
            let player = self.player.as_ref().ok_or("preview video is not open")?;
            return self.rest.get().map_or_else(|| player_position(player), Ok);
        }
        let audio_position = self.audio.position();
        if self.audio.is_playing() {
            if let Some(video_position) = self.audio_stalled(audio_position)? {
                return Ok(video_position);
            }
        }
        if self.audio.is_playing()
            && !self.past_video_end(audio_position)
            && self.last_drift_check.get().elapsed() >= Duration::from_millis(500)
        {
            if let Some(player) = &self.player {
                let video_position = player_position(player)?;
                if (video_position - audio_position).abs() > 0.075 {
                    self.place_video(audio_position)?;
                    self.drift_seeks.set(self.drift_seeks.get() + 1);
                }
            }
            self.last_drift_check.set(Instant::now());
        }
        Ok(audio_position)
    }

    /// The audio device stopped consuming (its clock froze while playing): let the picture run
    /// the clock instead of dragging the video back to a frozen time, and restart the sound
    /// at the picture's time (at most once a second). Returns the video time while stalled.
    fn audio_stalled(&self, audio_position: f64) -> Result<Option<f64>, String> {
        const STALL: Duration = Duration::from_millis(350);
        let now = Instant::now();
        let (seen, since) = self.audio_seen.get();
        if (audio_position - seen).abs() > 1e-6 {
            self.audio_seen.set((audio_position, now));
            return Ok(None);
        }
        if now.duration_since(since) < STALL {
            return Ok(None);
        }
        let player = self.player.as_ref().ok_or("preview video is not open")?;
        let video_position = player_position(player)?;
        if self.audio_restarted.get().elapsed() >= Duration::from_secs(1) {
            self.audio_restarted.set(now);
            self.audio_stalls.set(self.audio_stalls.get() + 1); // logged by the editor
            self.audio.seek(video_position)?;
            self.audio_seen.set((video_position, now));
        }
        Ok(Some(video_position))
    }

    /// Preview-audio output underruns since the preview was created (test trace, player log).
    pub fn audio_underruns(&self) -> u64 {
        if self.has_audio { self.audio.underruns() } else { 0 }
    }

    /// The audio clock without the drift correction `position` applies (test trace).
    pub fn audio_position(&self) -> f64 {
        if self.has_audio { self.audio.position() } else { self.video_position().unwrap_or(0.0) }
    }

    /// The video player's own state ("playing", "paused", "stopped", …) for the test trace.
    pub fn video_state(&self) -> Option<&'static str> {
        let player = self.player.as_ref()?;
        unsafe { player.GetState() }.ok().map(player_state_name)
    }

    pub fn video_position(&self) -> Result<f64, String> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        let player = self.player.as_ref().ok_or("preview video is not open")?;
        player_position(player)
    }

    pub fn set_audio(
        &mut self,
        stream_index: usize,
        gain_db: f64,
        muted: bool,
    ) -> Result<(), String> {
        // Kept for the sound coming back (`media_sound.rs`).
        self.sound.set_level(stream_index, gain_db, muted);
        if !self.has_audio {
            return if self.sound.is_down() { Ok(()) } else { Err("preview has no audio tracks".into()) };
        }
        self.audio.set_audio(stream_index, gain_db, muted)
    }

    fn require_ready(&self) -> Result<(), String> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        if !self.is_ready() {
            return Err("native preview is still loading".into());
        }
        Ok(())
    }
}
