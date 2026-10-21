use std::cell::Cell;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, RPC_E_CHANGED_MODE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, UpdateWindow,
};
use windows::Win32::Media::MediaFoundation::{
    IMFPMediaPlayer, IMFPMediaPlayerCallback, IMFPMediaPlayerCallback_Impl, MF_VERSION,
    MFP_EVENT_HEADER, MFP_EVENT_TYPE_ERROR, MFP_EVENT_TYPE_MEDIAITEM_SET, MFP_MEDIAPLAYER_STATE,
    MFP_MEDIAPLAYER_STATE_EMPTY, MFP_MEDIAPLAYER_STATE_PAUSED, MFP_MEDIAPLAYER_STATE_PLAYING,
    MFP_MEDIAPLAYER_STATE_SHUTDOWN, MFP_MEDIAPLAYER_STATE_STOPPED,
    MFP_OPTION_FREE_THREADED_CALLBACK, MFP_POSITIONTYPE_100NS, MFPCreateMediaPlayer,
    MFSTARTUP_FULL, MFStartup,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Variant::VT_I8;
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GWLP_WNDPROC,
    GetClientRect, GetParent, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, WM_PAINT, WM_SIZE, WNDPROC, WS_CHILD, WS_CLIPCHILDREN,
    WS_CLIPSIBLINGS, WS_EX_NOACTIVATE,
};
use windows::core::PCWSTR;
use windows_core::implement;

use crate::audio::AudioMixer;

static MEDIA_FOUNDATION: OnceLock<Result<(), String>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct PreviewTrack {
    pub stream_index: usize,
    pub path: String,
    pub gain_db: f64,
    pub muted: bool,
}

#[derive(Default)]
struct MediaStatus {
    ready: std::sync::atomic::AtomicBool,
    error: Mutex<Option<String>>,
}

#[derive(Default)]
struct PreviewWindowState {
    player: Mutex<Option<IMFPMediaPlayer>>,
    presentable: AtomicBool,
    previous_proc: AtomicIsize,
    paint_count: AtomicU64,
    update_count: AtomicU64,
    update_error: Mutex<Option<String>>,
}

impl PreviewWindowState {
    fn update_video(&self) {
        if !self.presentable.load(Ordering::Acquire) {
            return;
        }
        let result = self
            .player
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|player| unsafe { player.UpdateVideo() });
        if let Some(result) = result {
            match result {
                Ok(()) => {
                    self.update_count.fetch_add(1, Ordering::Relaxed);
                    *self.update_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
                Err(error) => {
                    *self.update_error.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(format!("could not repaint native video: {error}"));
                }
            }
        }
    }
}

unsafe extern "system" fn preview_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const PreviewWindowState;
    let state = unsafe { state_ptr.as_ref() };
    match message {
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            unsafe { BeginPaint(hwnd, &mut paint) };
            if let Some(state) = state {
                state.paint_count.fetch_add(1, Ordering::Relaxed);
                state.update_video();
            }
            unsafe { EndPaint(hwnd, &paint) };
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = state {
                state.update_video();
            }
            call_previous_window_proc(state, hwnd, message, wparam, lparam)
        }
        _ => call_previous_window_proc(state, hwnd, message, wparam, lparam),
    }
}

fn call_previous_window_proc(
    state: Option<&PreviewWindowState>,
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let previous = state.map_or(0, |state| state.previous_proc.load(Ordering::Acquire));
    if previous == 0 {
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    } else {
        let previous: WNDPROC = Some(unsafe { std::mem::transmute(previous) });
        unsafe { CallWindowProcW(previous, hwnd, message, wparam, lparam) }
    }
}

#[implement(IMFPMediaPlayerCallback)]
struct MediaCallback {
    status: Arc<MediaStatus>,
}

#[allow(non_snake_case)]
impl IMFPMediaPlayerCallback_Impl for MediaCallback_Impl {
    fn OnMediaPlayerEvent(&self, event: *const MFP_EVENT_HEADER) {
        let Some(event) = (unsafe { event.as_ref() }) else {
            return;
        };
        if event.hrEvent.is_err() || event.eEventType == MFP_EVENT_TYPE_ERROR {
            self.status
                .ready
                .store(false, std::sync::atomic::Ordering::Release);
            *self.status.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!(
                "Media Foundation preview failed (HRESULT 0x{:08x})",
                event.hrEvent.0 as u32
            ));
        } else if event.eEventType == MFP_EVENT_TYPE_MEDIAITEM_SET {
            self.status
                .ready
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

pub struct NativePreview {
    window: HWND,
    window_state: Box<PreviewWindowState>,
    player: Option<IMFPMediaPlayer>,
    callback: Option<IMFPMediaPlayerCallback>,
    media_status: Arc<MediaStatus>,
    audio: AudioMixer,
    has_audio: bool,
    com_initialized: bool,
    last_drift_check: Cell<Instant>,
    observed_media_ready: bool,
    observed_ready: bool,
    observed_error: Option<String>,
    prime_started: Option<Instant>,
}

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
            observed_media_ready: false,
            observed_ready: false,
            observed_error: None,
            prime_started: None,
        })
    }

    pub fn open(&mut self, path: &str, audio: &[PreviewTrack]) -> Result<(), String> {
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
        let url = wide(&normalized);
        let mut player = None;
        unsafe {
            MFPCreateMediaPlayer(
                PCWSTR(url.as_ptr()),
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
        if let Err(error) = unsafe { player.SetMute(true) } {
            let _ = unsafe { player.Shutdown() };
            return Err(format!("could not mute embedded source audio: {error}"));
        }
        self.has_audio = !audio.is_empty();
        if self.has_audio {
            if let Err(error) = self.audio.open(audio) {
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
        })
    }

    pub fn error(&self) -> Option<String> {
        self.media_status
            .error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .or_else(|| self.audio.error())
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
        self.require_ready()?;
        self.prime_started = None;
        let player = self.player.as_ref().ok_or("preview video is not open")?;
        if self.has_audio {
            set_player_position(player, self.audio.position())?;
        }
        unsafe { player.Play() }
            .map_err(|error| format!("could not play native video: {error}"))?;
        if self.has_audio {
            if let Err(error) = self.audio.play() {
                let _ = unsafe { player.Pause() };
                return Err(error);
            }
        }
        self.last_drift_check.set(Instant::now());
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), String> {
        self.prime_started = None;
        if let Some(player) = &self.player {
            if self.has_audio {
                self.audio.pause()?;
            }
            unsafe { player.Pause() }
                .map_err(|error| format!("could not pause native video: {error}"))?;
            if self.has_audio {
                set_player_position(player, self.audio.position())?;
            }
        }
        Ok(())
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
        set_player_position(player, seconds)?;
        self.last_drift_check.set(Instant::now());
        Ok(())
    }

    pub fn position(&self) -> Result<f64, String> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        if !self.has_audio {
            let player = self.player.as_ref().ok_or("preview video is not open")?;
            return player_position(player);
        }
        let audio_position = self.audio.position();
        if self.audio.is_playing()
            && self.last_drift_check.get().elapsed() >= Duration::from_millis(500)
        {
            if let Some(player) = &self.player {
                let video_position = player_position(player)?;
                if (video_position - audio_position).abs() > 0.075 {
                    set_player_position(player, audio_position)?;
                }
            }
            self.last_drift_check.set(Instant::now());
        }
        Ok(audio_position)
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
        if !self.has_audio {
            return Err("preview has no audio tracks".into());
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

impl Drop for NativePreview {
    fn drop(&mut self) {
        *self
            .window_state
            .player
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(player) = self.player.take() {
            let _ = unsafe { player.Shutdown() };
        }
        self.callback = None;
        let previous_proc = self.window_state.previous_proc.load(Ordering::Acquire);
        unsafe {
            if previous_proc != 0 {
                SetWindowLongPtrW(self.window, GWLP_WNDPROC, previous_proc);
            }
            SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
        }
        let _ = unsafe { DestroyWindow(self.window) };
        if self.com_initialized {
            unsafe { CoUninitialize() };
        }
    }
}

fn redraw_video_window(window: HWND, state: &PreviewWindowState) {
    state.update_video();
    unsafe {
        InvalidateRect(Some(window), None, false);
        UpdateWindow(window);
    }
}

fn rect_json(rect: RECT) -> serde_json::Value {
    serde_json::json!({
        "left": rect.left,
        "top": rect.top,
        "right": rect.right,
        "bottom": rect.bottom,
        "width": rect.right - rect.left,
        "height": rect.bottom - rect.top,
    })
}

fn player_state_name(state: MFP_MEDIAPLAYER_STATE) -> &'static str {
    if state == MFP_MEDIAPLAYER_STATE_EMPTY {
        "empty"
    } else if state == MFP_MEDIAPLAYER_STATE_STOPPED {
        "stopped"
    } else if state == MFP_MEDIAPLAYER_STATE_PLAYING {
        "playing"
    } else if state == MFP_MEDIAPLAYER_STATE_PAUSED {
        "paused"
    } else if state == MFP_MEDIAPLAYER_STATE_SHUTDOWN {
        "shutdown"
    } else {
        "unknown"
    }
}

fn set_player_position(player: &IMFPMediaPlayer, seconds: f64) -> Result<(), String> {
    let ticks = (seconds.max(0.0) * 10_000_000.0)
        .round()
        .min(i64::MAX as f64) as i64;
    let value = propvariant_i64(ticks);
    unsafe { player.SetPosition(&MFP_POSITIONTYPE_100NS, &value) }
        .map_err(|error| format!("could not seek native video: {error}"))
}

fn player_position(player: &IMFPMediaPlayer) -> Result<f64, String> {
    let value = unsafe { player.GetPosition(&MFP_POSITIONTYPE_100NS) }
        .map_err(|error| format!("could not read native video position: {error}"))?;
    let inner: &PROPVARIANT_0_0 = unsafe { &value.Anonymous.Anonymous };
    if inner.vt != VT_I8 {
        return Err(format!(
            "native video returned unexpected position type {}",
            inner.vt.0
        ));
    }
    let ticks = unsafe { inner.Anonymous.hVal };
    Ok(ticks as f64 / 10_000_000.0)
}

fn propvariant_i64(value: i64) -> PROPVARIANT {
    PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_I8,
                Anonymous: PROPVARIANT_0_0_0 { hVal: value },
                ..Default::default()
            }),
        },
    }
}

fn normalize_windows_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        path.to_owned()
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_extended_drive_prefix_for_mfplay() {
        assert_eq!(normalize_windows_path(r"\\?\C:\video.mp4"), r"C:\video.mp4");
    }

    #[test]
    fn converts_extended_unc_prefix_for_mfplay() {
        assert_eq!(
            normalize_windows_path(r"\\?\UNC\server\share\video.mp4"),
            r"\\server\share\video.mp4"
        );
    }

    #[test]
    fn builds_signed_100ns_position_variant() {
        let value = propvariant_i64(12_345);
        let inner: &PROPVARIANT_0_0 = unsafe { &value.Anonymous.Anonymous };
        assert_eq!(inner.vt, VT_I8);
        assert_eq!(unsafe { inner.Anonymous.hVal }, 12_345);
    }
}
