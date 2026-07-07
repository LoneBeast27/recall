//! Recall — capture engine.
//!
//! A live HDR->SDR source window (titled `<source> SDR`) that Discord / OBS
//! window-capture pick up. Pipeline:
//!   WGC capture (windows-capture, Rgba16F) -> GPU tonemap (wgpu) -> present.
//!
//! Driven manually via RECALL_* envs (human `[recall]` logs), or as a Tauri
//! sidecar via `--json` (machine protocol: JSON status on stdout, commands on
//! stdin). See `protocol.rs`.

mod border;
mod capture;
mod debug_dump;
mod gpu;
mod hdr;
mod protocol;
mod source;

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
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
}

fn parse_args() -> Config {
    let mut cfg = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => cfg.json = true,
            "--monitor" => cfg.monitor = args.next().and_then(|v| v.parse().ok()),
            _ => {}
        }
    }
    cfg
}

#[derive(Default)]
struct App {
    config: Config,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    shared: Option<Arc<CaptureShared>>,
    capture: Option<CaptureControlHandle>,
    source_name: String,
    poll_counter: u32,
    last_hdr: Option<bool>,
    max_nits: f32,
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

        // Announce version + monitors before capture starts (sidecar mode).
        if self.config.json {
            protocol::emit(&protocol::Outbound::Hello {
                version: env!("CARGO_PKG_VERSION").to_string(),
                monitors: capture::list_monitors(),
            });
            spawn_stdin_reader(shared.clone());
        }

        self.source_name = INITIAL_TITLE.trim_end_matches(" SDR").to_string();
        match start_monitor(self.config.monitor, shared.clone()) {
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
    fn tick_fps(&mut self) {
        self.fps_count += 1;
        let now = Instant::now();
        match self.fps_last {
            Some(last) if now.duration_since(last).as_secs_f32() >= 1.0 => {
                self.fps = self.fps_count;
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
        if let Some(name) = source::captured_source() {
            if name != self.source_name {
                self.source_name = name.clone();
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
                if info.is_hdr { "tonemap" } else { "passthrough" },
            );
        }
    }

    fn emit_status(&self) {
        if !self.config.json {
            return;
        }
        let Some(shared) = &self.shared else { return };
        let (width, height) = self.gpu.as_ref().and_then(|g| g.source_size()).unwrap_or((0, 0));
        let mode = if shared.mode.load(Relaxed) == capture::MODE_TONEMAP {
            "tonemap"
        } else {
            "passthrough"
        };
        protocol::emit(&protocol::Outbound::Status {
            display: if self.last_hdr == Some(true) { "hdr" } else { "sdr" }.to_string(),
            max_nits: self.max_nits,
            mode: mode.to_string(),
            select: shared.select_label().to_string(),
            source: self.source_name.clone(),
            width,
            height,
            fps: self.fps,
        });
    }
}

/// Read newline-delimited JSON commands from stdin and apply them to shared state.
fn spawn_stdin_reader(shared: Arc<CaptureShared>) {
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
    let config = parse_args();
    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        config,
        ..Default::default()
    };
    event_loop.run_app(&mut app).expect("run app");
}
