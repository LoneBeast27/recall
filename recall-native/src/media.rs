//! Native video preview: an MFPlay player in a child window, audio from WinMM stems.
//! Window/COM glue and PROPVARIANT helpers live here; open/prime in `media_player.rs`,
//! transport and diagnostics in `media_transport.rs`.
use std::cell::Cell;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, UpdateWindow,
};
use windows::Win32::Media::MediaFoundation::{
    IMFPMediaPlayer, IMFPMediaPlayerCallback, IMFPMediaPlayerCallback_Impl, MFP_EVENT_HEADER,
    MFP_EVENT_TYPE_ERROR, MFP_EVENT_TYPE_MEDIAITEM_SET, MFP_MEDIAPLAYER_STATE,
    MFP_MEDIAPLAYER_STATE_EMPTY, MFP_MEDIAPLAYER_STATE_PAUSED, MFP_MEDIAPLAYER_STATE_PLAYING,
    MFP_MEDIAPLAYER_STATE_SHUTDOWN, MFP_MEDIAPLAYER_STATE_STOPPED, MFP_POSITIONTYPE_100NS,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::CoUninitialize;
use windows::Win32::System::Variant::VT_I8;
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GWLP_WNDPROC,
    GetWindowLongPtrW, SetWindowLongPtrW, WM_PAINT, WM_SIZE, WNDPROC,
};
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
    updating: AtomicBool,
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
        // UpdateVideo is a cross-apartment COM call whose modal wait pumps messages and
        // can deliver WM_PAINT back into this window re-entrantly. Never hold the lock
        // across it (std Mutex is not re-entrant: that deadlocked the UI thread), and
        // drop nested repaints while one is in flight.
        if self.updating.swap(true, Ordering::AcqRel) {
            return;
        }
        let player = self.player.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let result = player.map(|player| unsafe { player.UpdateVideo() });
        self.updating.store(false, Ordering::Release);
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
        // A pause that lands after the session stopped at end of stream is harmless.
        const MF_E_SESSION_PAUSEWHILESTOPPED: i32 = 0xC00D36FC_u32 as i32;
        if event.hrEvent.0 == MF_E_SESSION_PAUSEWHILESTOPPED {
            return;
        }
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

#[path = "media_player.rs"]
mod player;
#[path = "media_transport.rs"]
mod transport;

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
