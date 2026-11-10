//! HDR Testcard — a digital-colourimeter reference source for the Recall
//! display-manager test rig.
//!
//! Two jobs:
//!   1. **Fake game.** The window title is exactly "HDR Testcard"; the test rig
//!      allowlists that title so Recall's foreground game-detection fires and
//!      the HDR auto-flip path runs. `--fullscreen` makes it a convincing
//!      borderless-fullscreen "game" that also fills the capture.
//!   2. **Colourimeter reference.** It paints KNOWN linear scRGB values on
//!      screen. The Recall capture engine (`RECALL_DUMP=out.png`, in
//!      the repo's `recall/` crate) reads the screen back as scRGB-linear
//!      Rgba16F and prints per-channel min/max/mean + %pixels above 1.0. Known
//!      commanded input + measured captured output = closed-loop verification of
//!      HDR flips and gamut-clamp effects with no human eyes.
//!
//! At startup it prints ONE JSON line to stdout describing the mode, the surface
//! class actually obtained, and the patch values on screen, so an orchestrator
//! can correlate commanded vs measured. See `usage()` for invocation.
//!
//! Toolchain is pinned to Recall's: winit 0.30 ApplicationHandler + wgpu 29.

mod config;
mod gpu;

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

use crate::config::{parse_args, Config};
use crate::gpu::Gpu;

const TITLE: &str = "HDR Testcard";
const DEFAULT_SIZE: PhysicalSize<u32> = PhysicalSize::new(960, 540);

struct App {
    config: Config,
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    /// Emit the startup JSON exactly once, after the surface is known.
    announced: bool,
}

impl App {
    fn new(config: Config) -> Self {
        Self {
            config,
            window: None,
            gpu: None,
            announced: false,
        }
    }

    /// Print the single machine-readable startup line to stdout. Includes the
    /// surface class actually obtained so a reader knows whether commanded
    /// values >1.0 could survive to the display or were clamped.
    fn announce(&mut self) {
        if self.announced {
            return;
        }
        let Some(gpu) = &self.gpu else { return };
        let patches = self.config.mode.patches();
        let line = serde_json::json!({
            "mode": self.config.mode.label(),
            "surface": gpu.surface_kind().label(),
            "patches": patches,
        });
        println!("{line}");
        // Flush so an orchestrator reading a pipe sees it before any render lag.
        use std::io::Write;
        let _ = std::io::stdout().flush();
        self.announced = true;
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let fullscreen = if self.config.fullscreen {
            // Borderless fullscreen on the primary monitor — a convincing
            // "game" for foreground detection and a full-surface capture target.
            Some(Fullscreen::Borderless(event_loop.primary_monitor()))
        } else {
            None
        };

        let attrs = Window::default_attributes()
            .with_title(TITLE)
            .with_inner_size(DEFAULT_SIZE)
            .with_fullscreen(fullscreen);

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        self.gpu = Some(Gpu::new(window.clone(), self.config.mode.clone()));
        self.window = Some(window);
        self.announce();

        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.render();
                }
                // Testcard is static; a single redraw per present is plenty, but
                // request the next so resizes / HDR flips repaint promptly.
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

/// Human-readable usage, printed to stderr on `--help`.
fn usage() {
    eprintln!(
        "HDR Testcard — colourimeter reference source for Recall.\n\
         \n\
         USAGE:\n\
         \x20 hdr-testcard [--solid R,G,B] [--fullscreen]\n\
         \n\
         MODES:\n\
         \x20 (default)        4x2 grid of known linear scRGB patches.\n\
         \x20 --solid R,G,B    Fill the window with one linear value (floats,\n\
         \x20                  may exceed 1.0 for HDR). Makes the capture MEAN\n\
         \x20                  directly comparable to the commanded value.\n\
         \n\
         FLAGS:\n\
         \x20 --fullscreen     Borderless-fullscreen on the primary monitor.\n\
         \x20 ESC              Close the window.\n\
         \n\
         Prints one JSON line to stdout at startup describing mode, surface\n\
         class (rgba16f = HDR path, srgb8 = clamped), and patch values."
    );
}

fn main() {
    // Cheap --help before spinning up a window / GPU.
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        usage();
        return;
    }

    let config = parse_args();
    let event_loop = EventLoop::new().expect("create event loop");
    // Testcard is static, but Poll keeps ESC / resize responsive.
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(config);
    event_loop.run_app(&mut app).expect("run app");
}
