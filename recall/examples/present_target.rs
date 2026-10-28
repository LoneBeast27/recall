//! Dev capture target for engine cost checks: a borderless topmost window that presents a
//! moving test pattern at a high frame rate. engine-cost.ps1 places it so a single pixel
//! (black) is on screen at the bottom-left corner: a window entirely off screen, or under
//! the taskbar, counts as occluded and WGC sends no frames for it.
//!
//!   present_target [--fps 500] [--width W --height H] [--x X --y Y] [--monitor I] [--seconds N]
//!
//! `--monitor I` places the window at monitor I's top-left (+ --x/--y), for a visible run.
//!
//! Prints one JSON line when the window is up (`{"hwnd":…,"width":…,"height":…}`), then
//! `{"presented":…}` once a second. The pattern carries six known sRGB patches (100 px, left),
//! a bar moving 8 px per frame, and the frame number as 24 binary blocks of 48 px along the
//! top, so a recorded clip (1152x664 and up) can be checked for colour and frame continuity.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId, WindowLevel};

const SHADER: &str = r#"
struct P { frame: u32, width: f32, height: f32, _pad: f32 };
@group(0) @binding(0) var<uniform> p: P;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

// Patch values are sRGB-encoded bytes / 255 written to a non-sRGB swapchain, so the
// window's pixels are exactly these bytes.
fn swatch(i: u32) -> vec3<f32> {
    switch i {
        case 0u: { return vec3<f32>(1.0, 0.0, 0.0); }
        case 1u: { return vec3<f32>(0.0, 1.0, 0.0); }
        case 2u: { return vec3<f32>(0.0, 0.0, 1.0); }
        case 3u: { return vec3<f32>(1.0, 1.0, 1.0); }
        case 4u: { return vec3<f32>(128.0, 128.0, 128.0) / 255.0; }
        default: { return vec3<f32>(0.0, 0.0, 0.0); }
    }
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let x = pos.x;
    let y = pos.y;
    // Top strip: frame number, 24 blocks of 48 px, MSB first, white = 1.
    if (y < 48.0) {
        let b = u32(x / 48.0);
        if (b < 24u) {
            let bit = (p.frame >> (23u - b)) & 1u;
            return vec4<f32>(vec3<f32>(f32(bit)), 1.0);
        }
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    // Left column: six 100x100 colour patches.
    if (x < 100.0 && y >= 64.0 && y < 64.0 + 6.0 * 100.0) {
        return vec4<f32>(swatch(u32((y - 64.0) / 100.0)), 1.0);
    }
    // Moving bar, 8 px per frame.
    let bar = f32((p.frame * 8u) % u32(p.width));
    if (x >= bar && x < bar + 32.0) {
        return vec4<f32>(1.0, 1.0, 0.0, 1.0);
    }
    // Background: horizontal grey ramp + a slow vertical tint so content changes everywhere.
    let ramp = x / p.width;
    let tint = 0.25 + 0.25 * sin(f32(p.frame) * 0.02 + y / p.height * 6.28);
    return vec4<f32>(ramp, ramp * 0.8 + tint * 0.2, tint, 1.0);
}
"#;

struct Args {
    fps: u32,
    width: Option<u32>,
    height: Option<u32>,
    x: i32,
    y: i32,
    monitor: Option<usize>,
    seconds: Option<u64>,
}

fn parse() -> Args {
    let mut a = Args { fps: 500, width: None, height: None, x: -32000, y: -32000, monitor: None, seconds: None };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        match k.as_str() {
            "--fps" => a.fps = v.parse().unwrap_or(500),
            "--width" => a.width = v.parse().ok(),
            "--height" => a.height = v.parse().ok(),
            "--x" => a.x = v.parse().unwrap_or(-32000),
            "--y" => a.y = v.parse().unwrap_or(-32000),
            "--seconds" => a.seconds = v.parse().ok(),
            "--monitor" => a.monitor = v.parse().ok(),
            _ => {}
        }
    }
    a
}

struct Gfx {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    width: u32,
    height: u32,
}

impl Gfx {
    fn new(window: Arc<Window>, width: u32, height: u32) -> Self {
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("present-target"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .expect("device");
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let present_mode = [wgpu::PresentMode::Immediate, wgpu::PresentMode::Mailbox]
            .into_iter()
            .find(|m| caps.present_modes.contains(m))
            .unwrap_or(wgpu::PresentMode::Fifo);
        surface.configure(
            &device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width,
                height,
                present_mode,
                desired_maximum_frame_latency: 2,
                alpha_mode: caps.alpha_modes[0],
                view_formats: vec![],
            },
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pattern"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pattern"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
        });
        eprintln!("[present_target] {format:?} {present_mode:?}");
        Self { surface, device, queue, pipeline, uniform, bind, width, height }
    }

    fn draw(&self, frame: u32) -> bool {
        let mut p = [0u8; 16];
        p[0..4].copy_from_slice(&frame.to_le_bytes());
        p[4..8].copy_from_slice(&(self.width as f32).to_le_bytes());
        p[8..12].copy_from_slice(&(self.height as f32).to_le_bytes());
        self.queue.write_buffer(&self.uniform, 0, &p);
        use wgpu::CurrentSurfaceTexture as Cst;
        let tex = match self.surface.get_current_texture() {
            Cst::Success(t) | Cst::Suboptimal(t) => t,
            _ => return false,
        };
        let view = tex.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        tex.present();
        true
    }
}

struct App {
    args: Args,
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    frame: u32,
    presented: u64,
    next: Option<Instant>,
    last_report: Option<Instant>,
    started: Option<Instant>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let size = el.primary_monitor().map(|m| m.size()).unwrap_or(PhysicalSize::new(2560, 1440));
        let (w, h) = (self.args.width.unwrap_or(size.width), self.args.height.unwrap_or(size.height));
        let (x, y) = match self.args.monitor.and_then(|i| el.available_monitors().nth(i)) {
            Some(m) => {
                let p = m.position();
                eprintln!("[present_target] monitor {:?} at {p:?} size {:?}", m.name(), m.size());
                (p.x + self.args.x.max(0), p.y + self.args.y.max(0))
            }
            None => (self.args.x, self.args.y),
        };
        let attrs = Window::default_attributes()
            .with_title("Recall Present Target")
            .with_inner_size(PhysicalSize::new(w, h))
            .with_position(PhysicalPosition::new(x, y))
            .with_decorations(false)
            .with_skip_taskbar(true)
            .with_active(false)
            // Topmost: the one on-screen pixel (see engine-cost.ps1) must not sit under the
            // taskbar, or DWM counts the window as occluded and WGC stops sending frames.
            .with_window_level(WindowLevel::AlwaysOnTop);
        let window = Arc::new(el.create_window(attrs).expect("window"));
        let hwnd = match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        };
        self.gfx = Some(Gfx::new(window.clone(), w, h));
        println!("{{\"hwnd\":{hwnd},\"width\":{w},\"height\":{h}}}");
        self.window = Some(window);
        self.started = Some(Instant::now());
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                if self.args.seconds.is_some_and(|s| self.started.is_some_and(|t| now - t >= Duration::from_secs(s))) {
                    el.exit();
                    return;
                }
                let period = Duration::from_secs_f64(1.0 / f64::from(self.args.fps.max(1)));
                let next = *self.next.get_or_insert(now);
                if now < next {
                    std::thread::sleep(next - now);
                }
                self.next = Some(next + period).filter(|n| *n > Instant::now() - period * 4).or(Some(Instant::now()));
                if let Some(g) = &self.gfx {
                    if g.draw(self.frame) {
                        self.presented += 1;
                    }
                }
                self.frame = self.frame.wrapping_add(1) & 0x00FF_FFFF;
                let last = *self.last_report.get_or_insert(now);
                if now - last >= Duration::from_secs(1) {
                    println!("{{\"presented\":{}}}", self.presented);
                    self.presented = 0;
                    self.last_report = Some(now);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let args = parse();
    let el = EventLoop::new().expect("event loop");
    el.set_control_flow(ControlFlow::Poll);
    let mut app = App { args, window: None, gfx: None, frame: 0, presented: 0, next: None, last_report: None, started: None };
    el.run_app(&mut app).expect("run");
}
