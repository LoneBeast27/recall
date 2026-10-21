//! Recall — capture engine.
//!
//! A live HDR->SDR source window (titled `<source> SDR`) that Discord / OBS
//! window-capture pick up. Pipeline:
//!   WGC capture (windows-capture, Rgba16F) -> GPU tonemap (wgpu) -> present.
//!
//! Driven manually via RECALL_* envs (human `[recall]` logs), or as a Tauri
//! sidecar via `--json` (machine protocol: JSON status on stdout, commands on
//! stdin). See `protocol.rs`.

#[cfg(feature = "encode")]
mod audio;
mod border;
mod capture;
mod debug_dump;
mod edit_model;
#[cfg(feature = "encode")]
mod edit_audio;
#[cfg(feature = "encode")]
mod editor_media;
#[cfg(feature = "encode")]
mod encoder;
#[cfg(feature = "encode")]
mod export;
mod gpu;
mod hdr;
#[cfg(feature = "encode")]
mod moments;
mod protocol;
mod ring;
mod source;

use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowId};

use crate::capture::{start_monitor, CaptureControlHandle, CaptureShared};
use crate::gpu::Gpu;

const INITIAL_TITLE: &str = "Desktop SDR";
const SOURCE_POLL_FRAMES: u32 = 90;

/// Parsed command-line config.
#[derive(Default, Clone)]
struct Config {
    json: bool,
    monitor: Option<usize>,
    encoder_smoke: Option<String>,
    export_cfr: Option<(String, String, u32)>,
    prepare_editor: Option<(String, String)>,
    export_edit: Option<(String, String, u32, String)>,
}

fn parse_args() -> Config {
    let mut cfg = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => cfg.json = true,
            "--encoder-smoke" => cfg.encoder_smoke = args.next(),
            "--prepare-editor" => {
                cfg.prepare_editor = Some((args.next().unwrap_or_default(), args.next().unwrap_or_default()));
            }
            "--export-edit" => {
                let input = args.next().unwrap_or_default();
                let output = args.next().unwrap_or_default();
                let fps = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
                let project = args.next().unwrap_or_default();
                cfg.export_edit = Some((input, output, fps, project));
            }
            "--export-cfr" => {
                let input = args.next().unwrap_or_default();
                let output = args.next().unwrap_or_default();
                let fps = args.next().map(|s| s.parse().unwrap_or(0)).unwrap_or(60);
                cfg.export_cfr = Some((input, output, fps));
            }
            "--monitor" => cfg.monitor = args.next().and_then(|v| v.parse().ok()),
            _ => {}
        }
    }
    cfg
}

/// Stdin-thread hook that triggers a replay save (None when encode is off).
type SaveHook = Box<dyn Fn(Option<u32>) + Send>;
type ExportHook = Box<dyn Fn(String, Option<String>, Option<u32>, Option<edit_model::EditPlan>) + Send>;

#[derive(Default)]
struct App {
    config: Config,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    shared: Option<Arc<CaptureShared>>,
    capture: Option<CaptureControlHandle>,
    source_name: String,
    /// Source name mirror for the stdin thread's save-path naming.
    source_shared: Arc<Mutex<String>>,
    poll_counter: u32,
    last_hdr: Option<bool>,
    max_nits: f32,
    // replay encode
    last_encoded_pts: Option<u64>,
    last_encoded_count: u64,
    enc_fps: u32,
    buffer_capacity_secs: u32,
    #[cfg(feature = "encode")]
    encoder: Option<encoder::EncoderHandle>,
    // fps tracking
    fps_count: u32,
    fps_last: Option<Instant>,
    fps: u32,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let show = std::env::var("RECALL_SHOW").is_ok();

        let size = event_loop
            .primary_monitor()
            .map(|m| m.size())
            .unwrap_or(PhysicalSize::new(1920, 1080));

        let mut attrs = Window::default_attributes()
            .with_title(INITIAL_TITLE)
            .with_inner_size(size);
        attrs = if show {
            attrs.with_position(PhysicalPosition::new(40, 40))
        } else {
            attrs
                .with_decorations(false)
                .with_skip_taskbar(true)
                .with_position(PhysicalPosition::new(-32000, -32000))
                .with_active(false)
        };

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        let shared = CaptureShared::new();

        // RECALL_MODE back-compat -> mode selector.
        if let Ok(m) = std::env::var("RECALL_MODE") {
            let v = match m.as_str() {
                "tonemap" | "1" | "hdr" => "hdr",
                "passthrough" | "0" | "sdr" => "sdr",
                _ => "auto",
            };
            shared.set_select(v);
        }

        // Replay encoder (feature `encode`): continuous NVENC into a RAM ring.
        #[cfg(feature = "encode")]
        let save_hook: Option<SaveHook>;
        #[cfg(not(feature = "encode"))]
        let save_hook: Option<SaveHook> = None;
        #[cfg(feature = "encode")]
        let export_hook: Option<ExportHook>;
        #[cfg(not(feature = "encode"))]
        let export_hook: Option<ExportHook> = None;
        self.enc_fps = std::env::var("RECALL_FPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        #[cfg(feature = "encode")]
        {
            let buffer_secs: u32 = std::env::var("RECALL_BUFFER_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60);
            self.buffer_capacity_secs = buffer_secs;
            let out_dir = std::env::var("RECALL_OUT_DIR")
                .unwrap_or_else(|_| r"V:\ALL RECORDINGS".to_string());
            let json = self.config.json;
            let tracks = audio::start(buffer_secs, |msg| {
                if json {
                    protocol::emit(&protocol::Outbound::EncoderLog { message: msg });
                } else {
                    eprintln!("[recall] {msg}");
                }
            });
            let handle = encoder::spawn(
                encoder::Config {
                    fps: self.enc_fps,
                    buffer_secs,
                    bitrate: std::env::var("RECALL_BITRATE")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(25_000_000),
                    json: self.config.json,
                },
                tracks,
            );
            let cmds = handle.commands.clone();
            let src = self.source_shared.clone();
            let timeline = shared.clone();
            save_hook = Some(Box::new(move |secs: Option<u32>| {
                let name = src.lock().map(|s| s.clone()).unwrap_or_default();
                let path = encoder::clip_path(&out_dir, &name);
                let _ = cmds.send(encoder::SaveRequest {
                    path,
                    seconds: secs.unwrap_or(buffer_secs),
                    end_us: timeline.recording_now_us(),
                });
            }));
            self.encoder = Some(handle);
            export_hook = Some(spawn_export_worker(self.config.json));
        }

        // Announce version + monitors before capture starts (sidecar mode).
        if self.config.json {
            protocol::emit(&protocol::Outbound::Hello {
                version: env!("CARGO_PKG_VERSION").to_string(),
                monitors: capture::list_monitors(),
            });
            spawn_stdin_reader(shared.clone(), save_hook, export_hook);
        }

        self.source_name = std::env::var("RECALL_CAPTURE_NAME").unwrap_or_else(|_| INITIAL_TITLE.trim_end_matches(" SDR").to_string());
        if let Ok(mut source)=self.source_shared.lock() { *source=self.source_name.clone(); }
        let started = if let Some(hwnd)=std::env::var("RECALL_CAPTURE_HWND").ok().and_then(|v|v.parse::<usize>().ok()) {
            capture::start_window(hwnd, shared.clone())
        } else { start_monitor(self.config.monitor, shared.clone()) };
        match started {
            Ok(control) => self.capture = Some(control),
            Err(e) => eprintln!("failed to start capture: {e}"),
        }

        self.gpu = Some(Gpu::new(window.clone(), shared.clone()));
        self.shared = Some(shared);
        self.window = Some(window);

        self.refresh_hdr_mode();
        self.emit_status();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                // Honor a `quit` command from stdin.
                if self.shared.as_ref().is_some_and(|s| s.quit.load(Relaxed)) {
                    event_loop.exit();
                    return;
                }
                if let Some(gpu) = &mut self.gpu {
                    gpu.render();
                }
                #[cfg(feature = "encode")]
                self.encode_tick();
                self.tick_fps();
                self.periodic_update();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

impl App {
    /// Encode each newly uploaded capture once, retaining its native timestamp.
    /// A full ingress queue drops a frame instead of blocking capture or growing RAM.
    #[cfg(feature = "encode")]
    fn encode_tick(&mut self) {
        let Some(enc) = &self.encoder else { return };
        let Some(gpu) = &mut self.gpu else { return };
        let Some(pts_us) = gpu.source_pts_us() else {
            return;
        };
        if self.last_encoded_pts == Some(pts_us) {
            return;
        }
        let Some((w, h, rgba)) = gpu.encode_frame() else {
            return;
        };
        self.last_encoded_pts = Some(pts_us);
        if let Err(std::sync::mpsc::TrySendError::Disconnected(_)) =
            enc.frames.try_send(encoder::PacedFrame {
                idx: pts_us,
                width: w,
                height: h,
                rgba,
            })
        {
            enc.stats.backend.store(3, Relaxed);
            if let Ok(mut error) = enc.stats.error.lock() {
                *error = Some("NVENC recording thread stopped; restart Recall".into());
            }
        }
    }

    fn tick_fps(&mut self) {
        self.fps_count += 1;
        let now = Instant::now();
        match self.fps_last {
            Some(last) if now.duration_since(last).as_secs_f32() >= 1.0 => {
                #[cfg(feature = "encode")]
                {
                    let count = self
                        .encoder
                        .as_ref()
                        .map(|e| e.stats.encoded_frames.load(Relaxed))
                        .unwrap_or(0);
                    self.fps = ((count.saturating_sub(self.last_encoded_count)) as f64
                        / now.duration_since(last).as_secs_f64())
                        as u32;
                    self.last_encoded_count = count;
                }
                #[cfg(not(feature = "encode"))]
                {
                    self.fps = self.fps_count;
                }
                self.fps_count = 0;
                self.fps_last = Some(now);
            }
            None => self.fps_last = Some(now),
            _ => {}
        }
    }

    fn periodic_update(&mut self) {
        self.poll_counter = self.poll_counter.wrapping_add(1);
        if self.poll_counter % SOURCE_POLL_FRAMES != 0 {
            return;
        }
        self.refresh_source_title();
        self.refresh_hdr_mode();
        self.emit_status();
    }

    fn refresh_source_title(&mut self) {
        // A controller-named source (a window target, or the idle "Desktop"
        // capture) keeps its name; only an unnamed monitor capture follows the
        // topmost window.
        if std::env::var_os("RECALL_CAPTURE_HWND").is_some() || std::env::var_os("RECALL_CAPTURE_NAME").is_some() { return; }
        if let Some(name) = source::captured_source() {
            if name != self.source_name {
                self.source_name = name.clone();
                if let Ok(mut s) = self.source_shared.lock() {
                    *s = name.clone();
                }
                let title = format!("{name} SDR");
                if let Some(w) = &self.window {
                    w.set_title(&title);
                }
                if !self.config.json {
                    eprintln!("[recall] SDR source -> {title}");
                }
            }
        }
    }

    fn refresh_hdr_mode(&mut self) {
        let Some(info) = hdr::primary_hdr() else {
            return;
        };
        self.max_nits = info.max_nits;
        let changed = self.last_hdr != Some(info.is_hdr);
        self.last_hdr = Some(info.is_hdr);

        // Only auto-detect drives the mode; a forced selector is left alone.
        if let Some(shared) = &self.shared {
            if shared.auto.load(Relaxed) {
                let mode = if info.is_hdr {
                    capture::MODE_TONEMAP
                } else {
                    capture::MODE_PASSTHROUGH
                };
                shared.mode.store(mode, Relaxed);
            }
        }

        if changed && !self.config.json {
            eprintln!(
                "[recall] display: {} (max {:.0} nits) -> {}",
                if info.is_hdr { "HDR" } else { "SDR" },
                info.max_nits,
                if info.is_hdr {
                    "tonemap"
                } else {
                    "passthrough"
                },
            );
        }
    }

    fn emit_status(&self) {
        if !self.config.json {
            return;
        }
        let Some(shared) = &self.shared else { return };
        let (width, height) = self
            .gpu
            .as_ref()
            .and_then(|g| g.source_size())
            .unwrap_or((0, 0));
        let mode = if shared.mode.load(Relaxed) == capture::MODE_TONEMAP {
            "tonemap"
        } else {
            "passthrough"
        };
        let (buffer_secs, encoder_label, encoder_error) = self.encoder_status();
        protocol::emit(&protocol::Outbound::Status {
            display: if self.last_hdr == Some(true) {
                "hdr"
            } else {
                "sdr"
            }
            .to_string(),
            max_nits: self.max_nits,
            mode: mode.to_string(),
            select: shared.select_label().to_string(),
            source: self.source_name.clone(),
            width,
            height,
            fps: self.fps,
            buffer_secs,
            encoder: encoder_label,
            encoder_error,
            recording_mode: "vfr".to_string(),
            mic_muted: { #[cfg(feature="encode")] { audio::mic_muted() } #[cfg(not(feature="encode"))] { false } },
        });
    }

    #[cfg(feature = "encode")]
    fn encoder_status(&self) -> (f32, String, Option<String>) {
        let Some(enc) = &self.encoder else {
            return (0.0, "off".into(), None);
        };
        let label = match enc.stats.backend.load(Relaxed) {
            1 => "h264_nvenc",
            3 => "failed",
            _ => "starting",
        };
        let error = enc.stats.error.lock().ok().and_then(|e| e.clone());
        let elapsed_us = if label == "failed" {
            0
        } else if enc.stats.buffered_frames.load(Relaxed) > 0 {
            self.shared
                .as_ref()
                .and_then(|s| s.recording_now_us())
                .map(|end| end.saturating_sub(enc.stats.buffer_start_us.load(Relaxed)))
                .unwrap_or_else(|| enc.stats.buffered_us.load(Relaxed))
                .min(self.buffer_capacity_secs as u64 * 1_000_000)
        } else {
            0
        };
        (elapsed_us as f32 / 1_000_000.0, label.into(), error)
    }

    #[cfg(not(feature = "encode"))]
    fn encoder_status(&self) -> (f32, String, Option<String>) {
        (0.0, "off".into(), None)
    }
}

/// Read newline-delimited JSON commands from stdin and apply them to shared
/// state. `save` triggers a replay-buffer save (None when encode is off).
fn spawn_stdin_reader(
    shared: Arc<CaptureShared>,
    save: Option<SaveHook>,
    export: Option<ExportHook>,
) {
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<protocol::Command>(line) {
                Ok(protocol::Command::SetMode { value }) => shared.set_select(&value),
                Ok(protocol::Command::SaveClip { seconds }) => match &save {
                    Some(hook) => hook(seconds),
                    None => protocol::emit(&protocol::Outbound::SaveFailed {
                        reason: "engine built without the encode feature".to_string(),
                    }),
                },
                Ok(protocol::Command::ExportClip { path, output, fps, edit }) => match &export {
                    Some(hook) => hook(path, output, fps, edit),
                    None => protocol::emit(&protocol::Outbound::ExportFailed {
                        source: path,
                        reason: "engine built without export support".into(),
                    }),
                },
                Ok(protocol::Command::ToggleMicMute) => {
                    #[cfg(feature="encode")]
                    protocol::emit(&protocol::Outbound::MicMuted { muted: audio::toggle_mic_mute() });
                }
                Ok(protocol::Command::SetMicMute { muted }) => {
                    #[cfg(feature="encode")]
                    { audio::set_mic_muted(muted); protocol::emit(&protocol::Outbound::MicMuted { muted }); }
                }
                Ok(protocol::Command::Quit) => {
                    shared.quit.store(true, Relaxed);
                    break;
                }
                Err(_) => {} // ignore malformed lines
            }
        }
    });
}

fn main() {
    if let Ok(mask)=std::env::var("RECALL_CPU_MASK") {
        use windows::Win32::System::Threading::{GetCurrentProcess,SetProcessAffinityMask,SetPriorityClass,BELOW_NORMAL_PRIORITY_CLASS};
        let parsed=usize::from_str_radix(mask.trim_start_matches("0x"),16).unwrap_or(0);
        if parsed==0 || unsafe { SetProcessAffinityMask(GetCurrentProcess(),parsed) }.is_err() {
            eprintln!("Recall CPU affinity could not be applied"); std::process::exit(2);
        }
        let _=unsafe { SetPriorityClass(GetCurrentProcess(),BELOW_NORMAL_PRIORITY_CLASS) };
    }
    let config = parse_args();
    #[cfg(feature = "encode")]
    if let Some((ref input, ref cache)) = config.prepare_editor {
        match editor_media::prepare_clip(input, cache) {
            Ok(info) => { println!("{}", serde_json::to_string(&info).expect("serialize media info")); return; }
            Err(e) => { eprintln!("prepare editor: {e}"); std::process::exit(1); }
        }
    }
    #[cfg(feature = "encode")]
    if let Some((ref input, ref output, fps, ref project)) = config.export_edit {
        let plan = std::fs::read_to_string(project).map_err(|e| e.to_string()).and_then(|s| {
            let value: serde_json::Value = serde_json::from_str(&s).map_err(|e| e.to_string())?;
            serde_json::from_value::<edit_model::EditPlan>(value.get("plan").cloned().unwrap_or(value)).map_err(|e| e.to_string())
        });
        let plan = match plan { Ok(p) => p, Err(e) => { eprintln!("editor project: {e}"); std::process::exit(1); } };
        let request = export::ExportRequest { input: input.clone(), output: output.clone(), fps };
        if run_export(&request, Some(&plan), config.json).is_err() { std::process::exit(1); }
        return;
    }
    #[cfg(feature = "encode")]
    if let Some(ref dir) = config.encoder_smoke {
        match encoder::smoke_test(std::path::Path::new(dir)) {
            Ok(path) => {
                println!("{path}");
                return;
            }
            Err(e) => {
                eprintln!("NVENC smoke failed: {e}");
                std::process::exit(1);
            }
        }
    }
    #[cfg(feature = "encode")]
    if let Some((ref input, ref output, fps)) = config.export_cfr {
        let request = export::ExportRequest {
            input: input.clone(),
            output: output.clone(),
            fps,
        };
        if run_export(&request, None, config.json).is_err() {
            std::process::exit(1);
        }
        return;
    }
    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        config,
        ..Default::default()
    };
    event_loop.run_app(&mut app).expect("run app");
}

/// A single bounded export worker keeps re-encoding separate from live recording.
#[cfg(feature = "encode")]
fn spawn_export_worker(json: bool) -> ExportHook {
    struct Job { request: export::ExportRequest, edit: Option<edit_model::EditPlan> }
    let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(1);
    std::thread::Builder::new()
        .name("recall-export".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                let _ = run_export(&job.request, job.edit.as_ref(), json);
            }
        })
        .expect("spawn export worker");
    Box::new(move |input, output, fps, edit| {
        let fps = fps.unwrap_or(60);
        let output = output.unwrap_or_else(|| {
            let source = std::path::Path::new(&input);
            let parent = source.parent().unwrap_or_else(|| std::path::Path::new("."));
            let stem = source
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Recall");
            parent
                .join("exports")
                .join(format!("{stem}-cfr-{fps}fps-{}.mp4", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .into_owned()
        });
        if let Some(parent) = std::path::Path::new(&output).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                export_failure(json, &input, &format!("create export directory: {e}"));
                return;
            }
        }
        let request = export::ExportRequest {
            input: input.clone(),
            output,
            fps,
        };
        if let Err(e) = tx.try_send(Job { request, edit }) {
            export_failure(json, &input, &format!("export queue unavailable: {e}"));
        }
    })
}

#[cfg(feature = "encode")]
fn export_failure(json: bool, source: &str, reason: &str) {
    if json {
        protocol::emit(&protocol::Outbound::ExportFailed {
            source: source.into(),
            reason: reason.into(),
        });
    } else {
        eprintln!("[recall] export failed: {reason}");
    }
}

#[cfg(feature = "encode")]
fn run_export(request: &export::ExportRequest, edit: Option<&edit_model::EditPlan>, json: bool) -> Result<(), String> {
    let start = Instant::now();
    if json {
        protocol::emit(&protocol::Outbound::ExportStarted {
            source: request.input.clone(),
            path: request.output.clone(),
            fps: request.fps,
        });
    }
    let result = match edit { Some(plan) => export::export_edited(request, plan), None => export::export_cfr(request) };
    match result {
        Ok(result) => {
            if json {
                protocol::emit(&protocol::Outbound::Exported {
                    source: request.input.clone(),
                    path: request.output.clone(),
                    fps: request.fps,
                    frames: result.frames,
                    seconds: result.seconds,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
            } else {
                println!(
                    "[recall] exported {} at {} CFR fps",
                    request.output, request.fps
                );
            }
            Ok(())
        }
        Err(e) => {
            export_failure(json, &request.input, &e);
            Err(e)
        }
    }
}
