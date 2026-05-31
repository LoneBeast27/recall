//! Recall — capture core.
//!
//! Phase 0: a live HDR->SDR source window (titled `<Game> SDR`) that Discord
//! screen-share / OBS window-capture can pick up. Pipeline:
//!   WGC capture (windows-capture, Rgba16F) -> GPU tonemap (wgpu) -> present (winit).

mod border;
mod capture;
mod debug_dump;
mod gpu;
mod hdr;
mod source;

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowId};

use crate::capture::{start_primary_monitor, CaptureControlHandle, CaptureShared};
use crate::gpu::Gpu;

/// Title before any foreground source is detected. Deliberately neutral — the
/// live SDR window carries no "Recall" branding (that's the clipping side).
const INITIAL_TITLE: &str = "Desktop SDR";

/// Foreground source is re-checked every this many frames (~1.5s at 60fps).
const SOURCE_POLL_FRAMES: u32 = 90;

#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    shared: Option<Arc<CaptureShared>>,
    /// Keeps the capture thread alive; dropping it stops capture.
    capture: Option<CaptureControlHandle>,
    /// Current `<source> SDR` window title and a frame counter for polling.
    source_title: String,
    poll_counter: u32,
    /// True when RECALL_MODE forces the mode (suppresses HDR auto-detect).
    mode_override: bool,
    /// Last detected HDR state, to log/switch only on change.
    last_hdr: Option<bool>,
}

impl App {
    /// Gated periodic work: refresh the source title and the HDR-derived mode.
    fn periodic_update(&mut self) {
        self.poll_counter = self.poll_counter.wrapping_add(1);
        if self.poll_counter % SOURCE_POLL_FRAMES != 0 {
            return;
        }
        self.refresh_source_title();
        self.refresh_hdr_mode();
    }

    /// Retitle the window to `<source> SDR` for the topmost window on the
    /// captured monitor. Keeps the last good title when nothing qualifies, so
    /// clicking a secondary-monitor app doesn't rename the source.
    fn refresh_source_title(&mut self) {
        if let Some(name) = source::captured_source() {
            let new_title = format!("{name} SDR");
            if new_title != self.source_title {
                self.source_title = new_title.clone();
                if let Some(w) = &self.window {
                    w.set_title(&new_title);
                }
                eprintln!("[recall] SDR source -> {new_title}");
            }
        }
    }

    /// Switch passthrough/tonemap from the primary display's HDR state (unless
    /// RECALL_MODE forces it). Logs on change so toggling HDR is visible.
    fn refresh_hdr_mode(&mut self) {
        if self.mode_override {
            return;
        }
        let Some(info) = hdr::primary_hdr() else {
            return;
        };
        if self.last_hdr == Some(info.is_hdr) {
            return;
        }
        self.last_hdr = Some(info.is_hdr);
        let mode = if info.is_hdr {
            capture::MODE_TONEMAP
        } else {
            capture::MODE_PASSTHROUGH
        };
        if let Some(shared) = &self.shared {
            shared
                .mode
                .store(mode, std::sync::atomic::Ordering::Relaxed);
        }
        eprintln!(
            "[recall] display: {} (max {:.0} nits) -> {}",
            if info.is_hdr { "HDR" } else { "SDR" },
            info.max_nits,
            if info.is_hdr { "tonemap" } else { "passthrough" },
        );
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // RECALL_SHOW makes the window visible/on-screen for debugging.
        let show = std::env::var("RECALL_SHOW").is_ok();

        // Size the window to the captured monitor so the output is 1:1 (native
        // resolution, no scaling -> crisp). Falls back to 1080p if unknown.
        let size = event_loop
            .primary_monitor()
            .map(|m| m.size())
            .unwrap_or(PhysicalSize::new(1920, 1080));

        self.source_title = INITIAL_TITLE.to_string();
        let mut attrs = Window::default_attributes()
            .with_title(INITIAL_TITLE)
            .with_inner_size(size);
        attrs = if show {
            attrs.with_position(PhysicalPosition::new(40, 40))
        } else {
            // Headless: a real, capturable window parked off the visible desktop
            // and hidden from taskbar/alt-tab. Discord/OBS still list it by title,
            // and being off the captured monitor avoids self-capture recursion.
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
        // Debug override: RECALL_MODE=tonemap|passthrough forces the mode until
        // the DXGI HDR auto-detect lands.
        if let Ok(m) = std::env::var("RECALL_MODE") {
            let mode = match m.as_str() {
                "tonemap" | "1" => capture::MODE_TONEMAP,
                _ => capture::MODE_PASSTHROUGH,
            };
            shared.mode.store(mode, std::sync::atomic::Ordering::Relaxed);
        }
        match start_primary_monitor(shared.clone()) {
            Ok(control) => self.capture = Some(control),
            Err(e) => eprintln!("failed to start capture: {e}"),
        }

        self.mode_override = std::env::var("RECALL_MODE").is_ok();
        self.gpu = Some(Gpu::new(window.clone(), shared.clone()));
        self.shared = Some(shared);
        self.window = Some(window);

        // Detect HDR once at startup so the mode + log are correct immediately.
        self.refresh_hdr_mode();
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
                if let Some(gpu) = &mut self.gpu {
                    gpu.render();
                }
                self.periodic_update();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("create event loop");
    // Poll so we keep pulling fresh capture frames; present_mode Fifo paces us to vsync.
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::default();
    event_loop.run_app(&mut app).expect("run app");
}
