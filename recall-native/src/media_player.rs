//! `NativePreview` lifecycle: COM/MF start-up, the child window, opening a clip and
//! priming its first frame.
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
use windows::Win32::Media::MediaFoundation::{
    IMFPMediaPlayer, IMFPMediaPlayerCallback, MF_VERSION, MFP_MEDIAPLAYER_STATE_PAUSED,
    MFP_MEDIAPLAYER_STATE_PLAYING, MFP_MEDIAPLAYER_STATE_STOPPED,
    MFP_OPTION_FREE_THREADED_CALLBACK, MFPCreateMediaPlayer, MFSTARTUP_FULL, MFStartup,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GWLP_USERDATA, GWLP_WNDPROC, SetWindowLongPtrW, WS_CHILD,
    WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_NOACTIVATE,
};
use windows::core::PCWSTR;

use super::{
    MEDIA_FOUNDATION, MediaCallback, MediaStatus, NativePreview, PreviewTrack, PreviewWindowState,
    StreamMedia, VideoLimits, normalize_windows_path, player_position, player_state_name, preview_window_proc,
    redraw_video_window, wide,
};
use crate::audio::AudioMixer;
use crate::audio::stream::LiveTrack;
use crate::services::storage::fill::Outcome;

impl NativePreview {
    pub fn new(parent_hwnd: isize) -> Result<Self, String> {
        if parent_hwnd == 0 {
            return Err("preview parent window handle is null".into());
        }
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let com_initialized = hr.is_ok();
        if hr.is_err() && hr != RPC_E_CHANGED_MODE {
            return Err(format!(
                "could not initialize COM for preview (HRESULT 0x{:08x})",
                hr.0 as u32
            ));
        }
        let mf_result = MEDIA_FOUNDATION.get_or_init(|| unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)
                .map_err(|error| format!("could not start Media Foundation: {error}"))
        });
        if let Err(error) = mf_result {
            if com_initialized {
                unsafe { CoUninitialize() };
            }
            return Err(error.clone());
        }
        let class = wide("STATIC");
        let window = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE,
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                Some(HWND(parent_hwnd as *mut _)),
                None,
                None,
                None,
            )
        }
        .map_err(|error| format!("could not create native preview window: {error}"))?;
        let window_state = Box::new(PreviewWindowState::default());
        unsafe {
            SetWindowLongPtrW(
                window,
                GWLP_USERDATA,
                (&*window_state as *const PreviewWindowState) as isize,
            );
        }
        let previous_proc = unsafe {
            SetWindowLongPtrW(
                window,
                GWLP_WNDPROC,
                preview_window_proc as *const () as isize,
            )
        };
        if previous_proc == 0 {
            unsafe {
                SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            }
            let _ = unsafe { DestroyWindow(window) };
            if com_initialized {
                unsafe { CoUninitialize() };
            }
            return Err("could not attach the native preview paint handler".into());
        }
        window_state
            .previous_proc
            .store(previous_proc, Ordering::Release);
        let audio = match AudioMixer::new() {
            Ok(audio) => audio,
            Err(error) => {
                let _ = unsafe { DestroyWindow(window) };
                if com_initialized {
                    unsafe { CoUninitialize() };
                }
                return Err(error);
            }
        };
        Ok(Self {
            window,
            window_state,
            player: None,
            callback: None,
            media_status: Arc::new(MediaStatus::default()),
            audio,
            has_audio: false,
            sound: Default::default(),
            com_initialized,
            last_drift_check: Cell::new(Instant::now()),
            drift_seeks: Cell::new(0),
            audio_seen: Cell::new((0.0, Instant::now())),
            audio_restarted: Cell::new(Instant::now()),
            audio_stalls: Cell::new(0),
            observed_media_ready: false,
            observed_ready: false,
            observed_error: None,
            prime_started: None,
            video_end: None,
            last_frame: None,
            rest: Cell::new(None),
            stream: None,
        })
    }

    /// Opens `path` with `audio` stems; `duration` is the clip's length on the editor timeline
    /// (the sound clock runs that long even when a stem is shorter). `limits` from
    /// `services::library::video_limits`.
    pub fn open(&mut self, path: &str, audio: &[PreviewTrack], duration: f64, limits: VideoLimits) -> Result<(), String> {
        if path.is_empty() {
            return Err("preview video path is empty".into());
        }
        for track in audio {
            if !track.gain_db.is_finite() {
                return Err(format!(
                    "preview track {} gain must be finite",
                    track.stream_index
                ));
            }
        }
        let (player, callback) = self.new_player(limits)?;
        let url = wide(&normalize_windows_path(path));
        // The callback selects the streams and sets the item (see `set_picture_only_item`).
        if let Err(error) = unsafe { player.CreateMediaItemFromURL(PCWSTR(url.as_ptr()), false, 0, None) } {
            let _ = unsafe { player.Shutdown() };
            return Err(format!("could not open native video preview: {error}"));
        }
        if let Err(error) = unsafe { player.SetMute(true) } {
            let _ = unsafe { player.Shutdown() };
            return Err(format!("could not mute embedded source audio: {error}"));
        }
        self.has_audio = !audio.is_empty();
        if self.has_audio {
            if let Err(error) = self.audio.open(audio, duration) {
                let _ = unsafe { player.Shutdown() };
                self.has_audio = false;
                return Err(error);
            }
            self.sound.opened(Some(super::sound::SoundSpec::Stems(audio.to_vec(), duration)));
        }
        self.keep_player(player, callback);
        Ok(())
    }

    /// Opens a clip that is still loading. `audio` carries each track's level; the sound comes
    /// from `media.sound`, or from the stems later ([`NativePreview::use_stems`]) without it.
    pub fn open_stream(&mut self, media: StreamMedia, audio: &[PreviewTrack], duration: f64) -> Result<(), String> {
        let (player, callback) = match self.new_player((media.map.video_end, media.map.last_frame)) {
            Ok(made) => made,
            Err(error) => {
                media.shut();
                return Err(error);
            }
        };
        if let Err(error) = unsafe { player.CreateMediaItemFromObject(&media.video.0, false, 0, None) } {
            let _ = unsafe { player.Shutdown() };
            media.shut();
            return Err(format!("could not open native video preview: {error}"));
        }
        let _ = unsafe { player.SetMute(true) };
        if let Some(sound) = &media.sound {
            let level = |index: usize| audio.iter().find(|track| track.stream_index == index);
            let tracks = sound.tracks.iter().map(|(index, pcm)| LiveTrack {
                stream_index: *index,
                pcm: Arc::clone(pcm),
                gain_db: level(*index).map_or(0.0, |track| track.gain_db),
                muted: level(*index).is_some_and(|track| track.muted),
            });
            self.has_audio = !sound.tracks.is_empty();
            if self.has_audio {
                let tracks: Vec<LiveTrack> = tracks.collect();
                self.sound.opened(Some(super::sound::SoundSpec::Live(tracks.clone(), duration)));
                if let Err(error) = self.audio.open_live(tracks, duration) {
                    self.has_audio = false;
                    let _ = unsafe { player.Shutdown() };
                    media.shut();
                    return Err(error);
                }
            }
        }
        media.fill.set_playing();
        self.keep_player(player, callback);
        self.stream = Some(media);
        Ok(())
    }

    /// The stems arrived for a clip whose sound could not be decoded as it loaded.
    pub fn use_stems(&mut self, audio: &[PreviewTrack], duration: f64) -> Result<(), String> {
        self.has_audio = !audio.is_empty();
        self.sound.opened(self.has_audio.then(|| super::sound::SoundSpec::Stems(audio.to_vec(), duration)));
        if self.has_audio { self.audio.open(audio, duration) } else { Ok(()) }
    }

    /// The picture on screen is the light copy, not full resolution yet.
    pub fn shows_light_copy(&self) -> bool {
        self.stream.as_ref().is_some_and(|media| media.light)
    }

    /// The clip's fill while it is loading.
    pub fn stream(&self) -> Option<&StreamMedia> {
        self.stream.as_ref()
    }

    /// The sound was to be decoded as the clip loads, and that failed or never started.
    pub fn sound_waits_for_stems(&self) -> bool {
        self.stream.as_ref().is_some_and(|media| media.sound.as_ref().is_none_or(|sound| sound.failed().is_some()))
    }

    /// Why the clip stopped loading, if it did.
    pub fn stream_failure(&self) -> Option<String> {
        let media = self.stream.as_ref()?;
        (media.fill.outcome() == Outcome::Failed).then(|| media.fill.error().unwrap_or_else(|| "The clip's drive stopped answering.".into()))
    }

    /// `[from, to)` seconds can play now: the bytes and the sound are there.
    pub fn stream_ready(&self, from: f64, to: f64) -> bool {
        let Some(media) = &self.stream else { return true };
        // A light copy's picture is all on the SSD: only the sound can wait.
        if !media.light && media.fill.outcome() != Outcome::Done {
            let (start, end) = media.map.bytes_of(from, to);
            if !media.fill.has(start, end) {
                return false;
            }
        }
        media.sound.as_ref().is_none_or(|sound| sound.failed().is_some() || sound.has(from, to))
    }

    /// Stops the copy and the decoder of the clip that was playing.
    pub(super) fn end_stream(&mut self) {
        if let Some(media) = &self.stream {
            if let Some(sound) = &media.sound {
                sound.stop();
            }
            media.fill.cancel();
        }
    }

    pub(super) fn shut_stream_source(&mut self) {
        if let Some(media) = self.stream.take() {
            let _ = unsafe { media.video.0.Shutdown() };
        }
    }

    /// Closes a clip that is still loading now, not when the next clip arrives: that may be a
    /// while on a slow drive, or never.
    pub fn close_stream(&mut self) -> Result<(), String> {
        if self.stream.is_some() { self.clear_clip() } else { Ok(()) }
    }

    /// Stops and lets go of the clip that was open.
    fn clear_clip(&mut self) -> Result<(), String> {
        self.end_stream();
        self.media_status = Arc::new(MediaStatus::default());
        self.has_audio = false;
        self.sound.opened(None);
        self.observed_media_ready = false;
        self.observed_ready = false;
        self.observed_error = None;
        self.prime_started = None;
        self.window_state
            .presentable
            .store(false, Ordering::Release);
        *self
            .window_state
            .update_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.audio.close()?;
        *self
            .window_state
            .player
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(old) = self.player.take() {
            let _ = unsafe { old.Shutdown() };
        }
        self.shut_stream_source();
        self.callback = None;
        Ok(())
    }

    /// Clears the clip that was open and makes a player for the next. `limits` is the new
    /// clip's (track end, start of the last frame).
    fn new_player(&mut self, limits: VideoLimits) -> Result<(IMFPMediaPlayer, IMFPMediaPlayerCallback), String> {
        self.clear_clip()?;
        let callback: IMFPMediaPlayerCallback = MediaCallback {
            status: Arc::clone(&self.media_status),
        }
        .into();
        (self.video_end, self.last_frame) = limits;
        self.rest.set(None);
        let mut player = None;
        unsafe {
            MFPCreateMediaPlayer(
                PCWSTR::null(),
                false,
                MFP_OPTION_FREE_THREADED_CALLBACK,
                &callback,
                Some(self.window),
                Some(&mut player),
            )
        }
        .map_err(|error| format!("could not open native video preview: {error}"))?;
        let player = player.ok_or_else(|| "Media Foundation returned no preview player".to_string())?;
        Ok((player, callback))
    }

    fn keep_player(&mut self, player: IMFPMediaPlayer, callback: IMFPMediaPlayerCallback) {
        *self.window_state.player.lock().unwrap_or_else(|e| e.into_inner()) = Some(player.clone());
        self.callback = Some(callback);
        self.player = Some(player);
        self.last_drift_check.set(Instant::now());
    }

    pub fn is_ready(&self) -> bool {
        self.media_status
            .ready
            .load(std::sync::atomic::Ordering::Acquire)
            && (!self.has_audio || self.audio.is_ready())
            && self.window_state.presentable.load(Ordering::Acquire)
    }

    pub fn refresh(&mut self) -> Result<bool, String> {
        // A sound failure leaves the picture playing; only a picture failure is an error.
        self.settle_sound();
        if let Some(sound) = self.stream.as_ref().and_then(|media| media.sound.as_ref()) {
            sound.set_playhead(if self.has_audio { self.audio.position() } else { 0.0 });
        }
        let error = self.error();
        let mut changed = error != self.observed_error;
        self.observed_error = error.clone();
        if let Some(error) = error {
            return Err(error);
        }

        let media_ready = self
            .media_status
            .ready
            .load(std::sync::atomic::Ordering::Acquire);
        if media_ready != self.observed_media_ready {
            self.observed_media_ready = media_ready;
            changed = true;
            if media_ready {
                let player = self.player.as_ref().ok_or("preview video is not open")?;
                let state = unsafe { player.GetState() }
                    .map_err(|error| format!("could not read native video state: {error}"))?;
                if state == MFP_MEDIAPLAYER_STATE_STOPPED {
                    unsafe { player.Play() }
                        .map_err(|error| format!("could not prime native video: {error}"))?;
                    self.prime_started = Some(Instant::now());
                } else if state == MFP_MEDIAPLAYER_STATE_PAUSED {
                    self.window_state.presentable.store(true, Ordering::Release);
                    redraw_video_window(self.window, &self.window_state);
                }
            }
        }

        if let Some(started) = self.prime_started {
            let player = self.player.as_ref().ok_or("preview video is not open")?;
            let state = unsafe { player.GetState() }
                .map_err(|error| format!("could not read native video state: {error}"))?;
            let advanced = player_position(player).is_ok_and(|position| position > 0.0);
            if state == MFP_MEDIAPLAYER_STATE_PLAYING
                && (advanced || started.elapsed() >= Duration::from_millis(100))
            {
                unsafe { player.Pause() }
                    .map_err(|error| format!("could not pause primed native video: {error}"))?;
                self.prime_started = None;
                self.window_state.presentable.store(true, Ordering::Release);
                redraw_video_window(self.window, &self.window_state);
                changed = true;
            } else if started.elapsed() >= if self.stream.is_some() { Duration::from_secs(30) } else { Duration::from_secs(2) } {
                self.prime_started = None;
                return Err(format!(
                    "native video did not start while priming the first frame (state {})",
                    player_state_name(state)
                ));
            }
        }

        let ready = self.is_ready();
        if ready != self.observed_ready {
            self.observed_ready = ready;
            changed = true;
        }
        Ok(changed)
    }
}
