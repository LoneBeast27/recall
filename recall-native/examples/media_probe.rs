//! Exercise Recall's actual native video child window and prepared-audio clock.
#[path = "../src/audio.rs"]
mod audio;
#[path = "../src/backend.rs"]
mod backend;
#[path = "../../recall/src/edit_model.rs"]
mod edit_model;
#[path = "../src/media.rs"]
mod media;

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use media::{NativePreview, PreviewTrack};
use serde_json::{Value, json};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
    SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, WM_QUIT, WS_CLIPCHILDREN, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW,
};
use windows::core::PCWSTR;

const READY_TIMEOUT: Duration = Duration::from_secs(8);

struct ParentWindow(HWND);

impl ParentWindow {
    fn create() -> Result<Self, String> {
        let class = wide("STATIC");
        let title = wide("Recall native media probe");
        let window = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                100,
                100,
                680,
                460,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(|error| format!("create probe parent window: {error}"))?;
        unsafe {
            ShowWindow(window, SW_SHOWNOACTIVATE);
        }
        Ok(Self(window))
    }
}

impl Drop for ParentWindow {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(source) = args.next() else {
        eprintln!("usage: media_probe <source.mp4> <output.json>");
        std::process::exit(2);
    };
    let Some(output) = args.next() else {
        eprintln!("usage: media_probe <source.mp4> <output.json>");
        std::process::exit(2);
    };
    if args.next().is_some() {
        eprintln!("usage: media_probe <source.mp4> <output.json>");
        std::process::exit(2);
    }

    let mut report = json!({
        "verdict": "FAIL",
        "source": source.clone(),
        "stages": [],
        "failure": null,
        "scope": "Win32 parent + MFPlay child + prepared WAV mixer; not GPUI composition"
    });
    let result = run_probe(&source, &mut report);
    let failed = result.is_err();
    match result {
        Ok(()) => report["verdict"] = json!("PASS"),
        Err(error) => report["failure"] = json!(error),
    }

    let write_result = std::fs::write(
        Path::new(&output),
        serde_json::to_vec_pretty(&report).expect("serialize probe report"),
    );
    if let Err(error) = write_result {
        eprintln!("write probe report '{output}': {error}");
        std::process::exit(3);
    }
    println!("{}", report);
    if failed {
        std::process::exit(1);
    }
}

fn run_probe(source: &str, report: &mut Value) -> Result<(), String> {
    let prepare_started = Instant::now();
    let prepared = backend::prepare_clip(source)?;
    push_stage(
        report,
        json!({
            "name": "prepared",
            "elapsed_ms": prepare_started.elapsed().as_millis(),
            "duration_seconds": prepared.info.duration,
            "audio_tracks": prepared.info.audio.len(),
            "warning": prepared.warning.clone(),
        }),
    );

    if !prepared.info.duration.is_finite() || prepared.info.duration <= 0.0 {
        return Err("prepared source has no positive finite duration".into());
    }
    if prepared.info.audio.is_empty() {
        return Err("prepared source has no audio tracks for the mixer clock".into());
    }

    let tracks = prepared
        .info
        .audio
        .iter()
        .map(|track| PreviewTrack {
            stream_index: track.stream_index,
            path: track.preview_path.clone(),
            gain_db: 0.0,
            muted: true,
        })
        .collect::<Vec<_>>();
    let parent = ParentWindow::create()?;
    let mut preview = NativePreview::new(parent.0.0 as isize)?;
    preview.open(&prepared.info.path, &tracks)?;
    preview.rect(0, 0, 640, 400)?;

    let ready_started = Instant::now();
    loop {
        pump_messages()?;
        preview.refresh()?;
        let diagnostics = preview.diagnostics();
        let priming = diagnostics["priming_first_frame"].as_bool().unwrap_or(true);
        if preview.is_ready() && !priming {
            push_stage(
                report,
                json!({
                    "name": "paused_initial",
                    "elapsed_ms": ready_started.elapsed().as_millis(),
                    "diagnostics": diagnostics,
                }),
            );
            break;
        }
        if ready_started.elapsed() >= READY_TIMEOUT {
            push_stage(
                report,
                json!({"name":"readiness_timeout","diagnostics":diagnostics}),
            );
            return Err(
                "native preview did not become ready with a primed frame within 8 seconds".into(),
            );
        }
        thread::sleep(Duration::from_millis(5));
    }

    let play_start_position = preview.position()?;
    preview.play()?;
    pump_for(&mut preview, Duration::from_millis(1100))?;
    preview.pause()?;
    pump_for(&mut preview, Duration::from_millis(100))?;
    let played_position = preview.position()?;
    let played_video_position = preview.video_position()?;
    push_stage(
        report,
        json!({
            "name": "played",
            "start_position_seconds": play_start_position,
            "position_seconds": played_position,
            "video_position_seconds": played_video_position,
            "diagnostics": preview.diagnostics(),
        }),
    );
    let audio_advance = played_position - play_start_position;
    let video_advance = played_video_position - play_start_position;
    if audio_advance < 0.5 || video_advance < 0.3 {
        return Err(format!(
            "native clocks did not advance during playback (audio {audio_advance:.3}s, video {video_advance:.3}s)"
        ));
    }

    let target = prepared.info.duration * 0.4;
    preview.seek(target)?;
    pump_for(&mut preview, Duration::from_millis(250))?;
    let seek_position = preview.position()?;
    let seek_video_position = preview.video_position()?;
    push_stage(
        report,
        json!({
            "name": "seeked_paused",
            "target_seconds": target,
            "position_seconds": seek_position,
            "video_position_seconds": seek_video_position,
            "diagnostics": preview.diagnostics(),
        }),
    );
    let audio_error = (seek_position - target).abs();
    let video_error = (seek_video_position - target).abs();
    if audio_error > 0.5 || video_error > 1.0 {
        return Err(format!(
            "native seek missed target {target:.3}s (audio error {audio_error:.3}s, video error {video_error:.3}s)"
        ));
    }

    drop(preview);
    drop(parent);
    Ok(())
}

fn pump_for(preview: &mut NativePreview, duration: Duration) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < duration {
        pump_messages()?;
        preview.refresh()?;
        let _ = preview.position()?;
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn pump_messages() -> Result<(), String> {
    let mut message = MSG::default();
    while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        if message.message == WM_QUIT {
            return Err("probe received WM_QUIT".into());
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn push_stage(report: &mut Value, stage: Value) {
    report["stages"]
        .as_array_mut()
        .expect("probe stages remain an array")
        .push(stage);
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
