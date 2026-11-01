//! `NativePreview` lifecycle: COM/MF start-up, the child window, opening a clip and
//! priming its first frame.
use std::cell::Cell;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
use windows::Win32::Media::MediaFoundation::{
    IMFPMediaPlayerCallback, MF_VERSION, MFP_MEDIAPLAYER_STATE_PAUSED,
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
    normalize_windows_path, player_position, player_state_name, preview_window_proc,
    redraw_video_window, wide,
};
use crate::audio::AudioMixer;

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
        })
    }

    /// Opens `path` with `audio` stems; `duration` is the clip's length on the editor timeline
    /// (the sound clock runs that long even when a stem is shorter).
    pub fn open(&mut self, path: &str, audio: &[PreviewTrack], duration: f64) -> Result<(), String> {
        if path.is_empty() {
            return Err("preview video path is empty".into());
        }
        if !Path::new(path).exists() {
            return Err(format!("preview video does not exist: {path}"));
        }
        for track in audio {
            if !track.gain_db.is_finite() {
                return Err(format!(
                    "preview track {} gain must be finite",
                    track.stream_index
                ));
            }
        }
        let status = Arc::new(MediaStatus::default());
        self.media_status = Arc::clone(&status);
        self.has_audio = false;
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
        self.callback = None;
        let callback: IMFPMediaPlayerCallback = MediaCallback {
            status: Arc::clone(&status),
        }
        .into();
        let normalized = normalize_windows_path(path);
        // MP4/MOV list the frames; for others (MKV) keep a tenth of a second clear of the end.
        let file = Path::new(&normalized);
        self.video_end = crate::services::library::mp4_video_duration(file).or(Some(duration));
        self.last_frame = crate::services::library::mp4_last_frame(file)
            .or_else(|| self.video_end.filter(|end| *end > 0.2).map(|end| end - 0.1));
        self.rest.set(None);
        let url = wide(&normalized);
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
        let player =
            player.ok_or_else(|| "Media Foundation returned no preview player".to_string())?;
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
        }
        *self
            .window_state
            .player
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(player.clone());
        self.callback = Some(callback);
        self.player = Some(player);
        self.last_drift_check.set(Instant::now());
        Ok(())
    }

    pub fn is_ready(&self) -> bool {
        self.media_status
            .ready
            .load(std::sync::atomic::Ordering::Acquire)
            && (!self.has_audio || self.audio.is_ready())
            && self.window_state.presentable.load(Ordering::Acquire)
    }

    pub fn refresh(&mut self) -> Result<bool, String> {
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
            } else if started.elapsed() >= Duration::from_secs(2) {
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
