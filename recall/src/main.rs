//! Recall — capture engine.
//!
//! Recording pipeline (zero-copy, `gpu_frames`): WGC capture (windows-capture, Rgba16F)
//! -> D3D11 tonemap on the capture device -> FFmpeg D3D11 frame -> NVENC. The event loop
//! then only ticks (status, screenshots) every 50 ms. Fallback (the GPU path cannot start,
//! or `RECALL_DUMP*`): CPU readback -> wgpu upload + tonemap -> present to the hidden
//! `<source> SDR` window -> wgpu readback -> NVENC, driven by a continuous redraw loop.
//!
//! Driven manually via RECALL_* envs (human `[recall]` logs), or as a Tauri
//! sidecar via `--json` (machine protocol: JSON status on stdout, commands on
//! stdin). See `protocol.rs`. The per-redraw [`App`] work lives in
//! `app_tick.rs`, the export worker in `export_worker.rs`.

/// The engine's `eprintln!`: written if it can be, never a panic (`stderr_log`).
/// Defined before every module so it shadows std's for the whole crate; the
/// controller's stderr pipe closes when it dies, and std's would panic then.
macro_rules! eprintln {
    () => {
        $crate::stderr_log::line(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::stderr_log::line(format_args!($($arg)*))
    };
}

mod stderr_log;
mod app_tick;
#[cfg(feature = "encode")]
mod audio;
#[cfg(feature = "encode")]
mod audio_clock;
mod bookmarks;
mod border;
mod capture;
mod capture_watchdog;
mod cpu_mask;
mod engine_extras;
#[cfg(feature = "encode")]
mod longrec;
mod naming;
mod screenshot;
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
#[cfg(feature = "encode")]
mod export_worker;
mod framing;
mod gpu;
#[cfg(feature = "encode")]
mod gpu_frames;
mod gpu_route;
#[cfg(test)]
mod hw_skip;
mod image_path;
mod hdr;
mod hud_probe;
mod hybrid_gpu;
mod protocol;
mod ring;
mod single_instance;
mod spill;
mod file_id;
mod win_volume;
mod thread_qos;
mod source;
mod stdin_commands;

use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowId};

use crate::capture::{CaptureControlHandle, CaptureShared};
#[cfg(feature = "encode")]
use crate::export_worker::{run_export, spawn_export_worker};
use crate::gpu::Gpu;
use crate::gpu_route::gpu_path_possible;
#[cfg(feature = "encode")]
use crate::gpu_route::{install_gpu_sink, GpuFramesTx};

const INITIAL_TITLE: &str = "Desktop SDR";
/// Status line, source title and HDR mode refresh cadence.
const PERIODIC: Duration = Duration::from_secs(1);
/// Event-loop tick while frames take the GPU path (quit, screenshots, status).
const IDLE_TICK: Duration = Duration::from_millis(50);

/// Parsed command-line config.
#[derive(Default, Clone)]
struct Config {
    json: bool,
    monitor: Option<usize>,
    encoder_smoke: Option<String>,
    gpu_smoke: Option<String>,
    export_cfr: Option<(String, String, u32)>,
    prepare_editor: Option<(String, String)>,
    export_edit: Option<(String, String, u32, String)>,
}

/// What this build answers to, for the app to read from the program file without running it
/// (a build that does not know a command starts as a recorder and never returns). Only `encode`
/// builds carry it: without encode neither command works.
#[cfg(feature = "encode")]
#[used]
static ENGINE_CAPS: [u8; 46] = *b"recall-engine-caps:prepare-editor,export-edit;";

const USAGE: &str = "usage: recall [--json] [--monitor N]   (records; one engine per user)
       recall --prepare-editor INPUT CACHE_DIR
       recall --export-edit INPUT OUTPUT FPS PROJECT.json [--json]
       recall --export-cfr INPUT OUTPUT [FPS] [--json]
       recall --encoder-smoke DIR | --gpu-smoke DIR
       recall --help";

/// What the command line asks for.
enum Args {
    Run(Config),
    Help,
    Bad(String),
}

fn parse_args() -> Args {
    parse_args_from(std::env::args().skip(1))
}

/// Unknown arguments are refused: an engine that ignored them would start recording.
fn parse_args_from(args: impl IntoIterator<Item = String>) -> Args {
    let mut cfg = Config::default();
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => cfg.json = true,
            "--encoder-smoke" => cfg.encoder_smoke = args.next(),
            "--gpu-smoke" => cfg.gpu_smoke = args.next(),
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
            "--help" | "-h" | "/?" | "-?" => return Args::Help,
            other => return Args::Bad(format!("unknown argument '{other}'")),
        }
    }
    Args::Run(cfg)
}

/// How long a new recorder waits for an engine that is still ending (a restart).
const ENGINE_WAIT: Duration = Duration::from_secs(3);
/// Exit code when another engine already records.
const EXIT_ALREADY_RUNNING: i32 = 3;

/// Claims the recorder guard. Only the recorder takes it: one-shot commands (prepare,
/// export, smoke tests) run beside a recording engine.
fn claim_recorder(json: bool) -> Option<single_instance::Guard> {
    let say = |message: String| {
        if json {
            protocol::emit(&protocol::Outbound::EncoderLog { message });
        } else {
            eprintln!("[recall] {message}");
        }
    };
    match single_instance::claim_within(&single_instance::mutex_name(single_instance::ENGINE), ENGINE_WAIT) {
        single_instance::Claim::First(guard) => Some(guard),
        single_instance::Claim::Taken => {
            say("another Recall engine is already recording; exiting".into());
            std::process::exit(EXIT_ALREADY_RUNNING);
        }
        single_instance::Claim::Unavailable(code) => {
            say(format!("single-instance guard unavailable (error {code}); recording anyway"));
            None
        }
    }
}

/// Stdin-thread hook that triggers a replay save (None when encode is off).
type SaveHook = Box<dyn Fn(Option<u32>, bool) + Send>;
type ExportHook = Box<dyn Fn(String, Option<String>, Option<u32>, Option<edit_model::EditPlan>) + Send>;

#[derive(Default)]
struct App {
    config: Config,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    shared: Option<Arc<CaptureShared>>,
    capture: Option<CaptureControlHandle>,
    /// Retry / restart backoff for a capture that failed or died (`capture_watchdog`).
    capture_watch: capture_watchdog::Backoff,
    source_name: String,
    /// Source name mirror for the stdin thread's save-path naming.
    source_shared: Arc<Mutex<String>>,
    last_periodic: Option<Instant>,
    next_tick: Option<Instant>,
    last_hdr: Option<bool>,
    max_nits: f32,
    // replay encode
    last_encoded_pts: Option<u64>,
    last_encoded_count: u64,
    enc_fps: u32,
    buffer_capacity_secs: u32,
    #[cfg(feature = "encode")]
    encoder: Option<encoder::EncoderHandle>,
    /// The capture thread's handle on the encoder queue (dropped at quit so it closes).
    #[cfg(feature = "encode")]
    gpu_frames_tx: Option<GpuFramesTx>,
    /// Hotkey features: screenshot requests (fulfilled on this thread) and
    /// the long-recording writer (finalized at quit).
    shots: Option<Arc<screenshot::Screenshots>>,
    #[cfg(feature = "encode")]
    longrec: Option<Arc<longrec::LongRec>>,
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

        // Bookmarks are held for the replay window (see `bookmarks`).
        let marks = Arc::new(bookmarks::Bookmarks::new(
            std::env::var("RECALL_BUFFER_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(60),
        ));

        // Replay encoder (feature `encode`): continuous NVENC into a RAM ring.
        #[cfg(feature = "encode")]
        let save_hook: Option<SaveHook>;
        #[cfg(not(feature = "encode"))]
        let save_hook: Option<SaveHook> = None;
        #[cfg(feature = "encode")]
        let export_hook: Option<ExportHook>;
        #[cfg(not(feature = "encode"))]
        let export_hook: Option<ExportHook> = None;
        #[cfg(feature = "encode")]
        {
            // RECALL_CODEC/_QUALITY/_BITRATE/_FPS/_BUFFER_SECS: encoder/settings.rs.
            let enc_cfg = encoder::Config::from_env(self.config.json);
            self.enc_fps = enc_cfg.fps;
            let buffer_secs = enc_cfg.buffer_secs;
            self.buffer_capacity_secs = buffer_secs;
            let out_dir = naming::out_dir();
            let json = self.config.json;
            let tracks = audio::start(buffer_secs, |msg| {
                if json {
                    protocol::emit(&protocol::Outbound::EncoderLog { message: msg });
                } else {
                    eprintln!("[recall] {msg}");
                }
            });
            let track_infos = tracks
                .iter()
                .map(|t| longrec::TrackInfo { name: t.name, rate: t.rate, channels: t.channels, boosted: t.boosted() })
                .collect();
            self.longrec = Some(Arc::new(longrec::spawn(out_dir.clone(), json, track_infos)));
            longrec::recover_parts(out_dir.clone(), json);
            let handle = encoder::spawn(enc_cfg, tracks);
            self.gpu_frames_tx = Some(install_gpu_sink(&shared, &handle, json));
            let cmds = handle.commands.clone();
            let src = self.source_shared.clone();
            let timeline = shared.clone();
            let save_marks = marks.clone();
            save_hook = Some(Box::new(move |secs: Option<u32>, at_last_frame: bool| {
                let name = src.lock().map(|s| s.clone()).unwrap_or_default();
                let path = encoder::clip_path(&out_dir, &name);
                let end_us = timeline.recording_now_us();
                if let Some(end) = end_us {
                    save_marks.note_save(&path, end);
                }
                let _ = cmds.send(encoder::SaveRequest {
                    path,
                    seconds: secs.unwrap_or(buffer_secs),
                    end_us,
                    at_last_frame,
                    requested: Instant::now(),
                });
            }));
            self.encoder = Some(handle);
            export_hook = Some(spawn_export_worker(self.config.json));
        }

        // Hotkey features: marks sidecar on `Saved`, screenshots, long recording.
        let sidecar_marks = marks.clone();
        protocol::set_observer(move |msg| bookmarks::on_emit(&sidecar_marks, msg));
        let shots = screenshot::spawn(naming::out_dir(), self.config.json);
        self.shots = Some(shots.clone());
        let extras = engine_extras::Extras {
            shared: shared.clone(),
            source: self.source_shared.clone(),
            bookmarks: marks,
            shots,
            hud: hud_probe::HudProbe::new(shared.clone(), self.config.json),
            #[cfg(feature = "encode")]
            longrec: self.longrec.clone(),
            json: self.config.json,
        };

        // Announce version + monitors before capture starts (sidecar mode).
        if self.config.json {
            protocol::emit(&protocol::Outbound::Hello {
                version: env!("CARGO_PKG_VERSION").to_string(),
                monitors: capture::list_monitors(),
                live_sources: true,
            });
            spawn_stdin_reader(shared.clone(), save_hook, export_hook, extras);
        }

        self.source_name = std::env::var("RECALL_CAPTURE_NAME").unwrap_or_else(|_| INITIAL_TITLE.trim_end_matches(" SDR").to_string());
        if let Ok(mut source)=self.source_shared.lock() { *source=self.source_name.clone(); }
        // The wgpu renderer is only for the CPU fallback: on the GPU path it would add a
        // device, a swapchain and a busy present loop for a window nobody sees.
        if !gpu_path_possible() {
            self.gpu = Some(Gpu::new(window.clone(), shared.clone()));
        }
        self.shared = Some(shared);
        self.window = Some(window);

        // The display mode picks the capture format (Bgra8 for SDR, fp16 for HDR).
        self.refresh_hdr_mode();
        self.start_capture();
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
            // CPU path only: each redraw renders, encodes and asks for the next one.
            WindowEvent::RedrawRequested => {
                let Some(gpu) = &mut self.gpu else { return };
                // Honor a `quit` command from stdin.
                if self.shared.as_ref().is_some_and(|s| s.quit.load(Relaxed)) {
                    event_loop.exit();
                    return;
                }
                gpu.render();
                #[cfg(feature = "encode")]
                self.encode_tick();
                self.screenshot_tick();
                self.tick_fps();
                self.periodic_update();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.idle_tick(event_loop);
    }
}

/// Read newline-delimited JSON commands from stdin and apply them to shared
/// state. `save` triggers a replay-buffer save (None when encode is off).
fn spawn_stdin_reader(
    shared: Arc<CaptureShared>,
    save: Option<SaveHook>,
    export: Option<ExportHook>,
    extras: engine_extras::Extras,
) {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        stdin_commands::read_commands(stdin.lock(), &shared.quit, |cmd| {
            match cmd {
                protocol::Command::SetMode { value } => shared.set_select(&value),
                protocol::Command::SaveClip { seconds, at_last_frame }=> match &save {
                    Some(hook) => hook(seconds, at_last_frame),
                    None => protocol::emit(&protocol::Outbound::SaveFailed {
                        reason: "engine built without the encode feature".to_string(),
                        rejected: true,
                    }),
                },
                protocol::Command::ExportClip { path, output, fps, edit }=> match &export {
                    Some(hook) => hook(path, output, fps, edit),
                    None => protocol::emit(&protocol::Outbound::ExportFailed {
                        source: path,
                        reason: "engine built without export support".into(),
                    }),
                },
                protocol::Command::ToggleMicMute => {
                    #[cfg(feature="encode")]
                    protocol::emit(&protocol::Outbound::MicMuted { muted: audio::toggle_mic_mute() });
                }
                protocol::Command::SetMicMute { muted } => {
                    #[cfg(feature="encode")]
                    { audio::set_mic_muted(muted); protocol::emit(&protocol::Outbound::MicMuted { muted }); }
                }
                protocol::Command::SetSource { id, gain_db, name } => {
                    #[cfg(feature="encode")]
                    {
                        if !audio::sources::set_live(&id, gain_db, name.as_deref()) {
                            eprintln!("[recall] set_source: no live track '{id}'");
                        }
                    }
                    #[cfg(not(feature="encode"))]
                    let _ = (id, gain_db, name);
                }
                cmd @ (protocol::Command::Bookmark
                    | protocol::Command::Screenshot
                    | protocol::Command::HudProbe
                    | protocol::Command::RecordStart
                    | protocol::Command::RecordStop
                    | protocol::Command::RecordToggle
                    | protocol::Command::RecordFail { .. }) => {
                    extras.handle(&cmd);
                }
                protocol::Command::Quit => {} // read_commands sets `quit`
            }
        });
    });
}

fn main() {
    #[cfg(feature = "encode")]
    std::hint::black_box(&ENGINE_CAPS); // survives release linking whatever `#[used]` keeps
    std::hint::black_box((&hybrid_gpu::NvOptimusEnablement, &hybrid_gpu::AmdPowerXpressRequestHighPerformance));
    if let Ok(mask) = std::env::var("RECALL_CPU_MASK") {
        cpu_mask::apply(&mask);
    }
    if std::env::var_os("RECALL_ECOQOS").is_some_and(|v| v == "1") {
        thread_qos::eco_process();
    }
    let config = match parse_args() {
        Args::Run(config) => config,
        Args::Help => {
            println!("{USAGE}");
            return;
        }
        Args::Bad(why) => {
            eprintln!("recall: {why}\n{USAGE}");
            std::process::exit(2);
        }
    };
    #[cfg(feature = "encode")]
    if let Some((ref input, ref cache)) = config.prepare_editor {
        match editor_media::prepare_clip(input, cache) {
            Ok(info) => { println!("{}", serde_json::to_string(&info).expect("serialize media info")); return; }
            Err(e) => { eprintln!("prepare editor: {e}"); std::process::exit(1); }
        }
    }
    #[cfg(feature = "encode")]
    if let Some((ref input, ref output, fps, ref project)) = config.export_edit {
        let project = std::fs::read_to_string(project).map_err(|e| e.to_string()).and_then(|s| export::parse_project(&s));
        let (plan, settings) = match project { Ok(p) => p, Err(e) => { eprintln!("editor project: {e}"); std::process::exit(1); } };
        let request = export::ExportRequest { input: input.clone(), output: output.clone(), fps };
        export_worker::standalone_edit(config.json);
        if run_export(&request, Some((&plan, &settings)), config.json).is_err() { std::process::exit(1); }
        return;
    }
    #[cfg(feature = "encode")]
    if let Some(ref dir) = config.gpu_smoke {
        match encoder::gpu_smoke(std::path::Path::new(dir)) {
            Ok(report) => {
                println!("{report}");
                return;
            }
            Err(e) => {
                eprintln!("GPU smoke failed: {e}");
                std::process::exit(1);
            }
        }
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
    // Held until the process ends: Windows frees it then, after a crash too.
    let _instance = claim_recorder(config.json);
    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        config,
        ..Default::default()
    };
    event_loop.run_app(&mut app).expect("run app");
    // Quit: finalize a running long recording first (a kill after this point
    // leaves a `.part` that the next start recovers), then let an in-flight
    // save finish (or remove its .part) inside the supervisor's grace.
    #[cfg(feature = "encode")]
    if let Some(longrec) = app.longrec.take() {
        longrec.shutdown(std::time::Duration::from_millis(1500));
    }
    // The capture thread's sender too, or the encoder's frame queue never closes.
    #[cfg(feature = "encode")]
    if let Some(tx) = app.gpu_frames_tx.take() {
        if let Ok(mut tx) = tx.lock() {
            tx.take();
        }
    }
    #[cfg(feature = "encode")]
    if let Some(encoder) = app.encoder.take() {
        encoder.shutdown(std::time::Duration::from_secs(3));
    }
}

#[cfg(test)]
mod args_tests {
    use super::{parse_args_from, Args};

    fn parse(args: &[&str]) -> Args {
        parse_args_from(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn the_commands_scripts_and_the_app_send_still_parse() {
        assert!(matches!(parse(&[]), Args::Run(_)));
        assert!(matches!(parse(&["--json"]), Args::Run(c) if c.json));
        assert!(matches!(parse(&["--export-edit", "in.mp4", "out.mp4", "60", "p.json", "--json"]), Args::Run(c) if c.json && c.export_edit.is_some()));
        assert!(matches!(parse(&["--prepare-editor", "in.mp4", "cache"]), Args::Run(c) if c.prepare_editor.is_some()));
        assert!(matches!(parse(&["--export-cfr", "in.mp4", "out.mp4", "30"]), Args::Run(c) if c.export_cfr.is_some()));
        assert!(matches!(parse(&["--gpu-smoke", "dir"]), Args::Run(c) if c.gpu_smoke.is_some()));
        assert!(matches!(parse(&["--monitor", "1"]), Args::Run(c) if c.monitor == Some(1)));
    }

    #[test]
    fn help_and_unknown_arguments_never_reach_the_recorder() {
        for flag in ["--help", "-h", "/?", "-?"] {
            assert!(matches!(parse(&[flag]), Args::Help), "{flag}");
        }
        assert!(matches!(parse(&["--json", "--help"]), Args::Help));
        assert!(matches!(parse(&["--recrod"]), Args::Bad(why) if why.contains("--recrod")));
        assert!(matches!(parse(&["stray"]), Args::Bad(_)));
    }
}
