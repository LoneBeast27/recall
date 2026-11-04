//! Per-redraw and periodic work of the engine [`App`]: the idle tick of the GPU path,
//! the encode and screenshot ticks, fps tracking, source-title / HDR-mode refresh, and
//! the `--json` status line.

use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use winit::event_loop::{ActiveEventLoop, ControlFlow};

#[cfg(feature = "encode")]
use crate::{audio, encoder};
use crate::framing::{self, Rect};
use crate::{capture, hdr, protocol, source};
use crate::{App, IDLE_TICK, PERIODIC};

/// A screenshot is the picture only: a windowed game's canvas bars are cut off.
fn picture(w: u32, h: u32, rgba: Vec<u8>, content: Option<Rect>) -> (u32, u32, Vec<u8>) {
    match content {
        Some(r) if r.clamp_to((w, h)) == Some(r) => framing::crop_rgba(w, &rgba, r),
        _ => (w, h, rgba),
    }
}

impl App {
    /// Encode each newly uploaded capture once, retaining its native timestamp.
    /// A full ingress queue drops a frame instead of blocking capture or growing RAM.
    #[cfg(feature = "encode")]
    pub(super) fn encode_tick(&mut self) {
        let Some(enc) = &self.encoder else { return };
        let Some(gpu) = &mut self.gpu else { return };
        let Some(pts_us) = gpu.source_pts_us() else {
            return;
        };
        if self.last_encoded_pts == Some(pts_us) {
            return;
        }
        let Some((w, h, rgba, content)) = gpu.encode_frame() else {
            return;
        };
        self.last_encoded_pts = Some(pts_us);
        if let Err(std::sync::mpsc::TrySendError::Disconnected(_)) =
            enc.frames.try_send(encoder::PacedFrame {
                idx: pts_us,
                width: w,
                height: h,
                rgba,
                gpu: None,
                content,
            })
        {
            enc.stats.backend.store(3, Relaxed);
            if let Ok(mut error) = enc.stats.error.lock() {
                *error = Some("NVENC recording thread stopped; restart Recall".into());
            }
        }
    }

    /// A pending screenshot reads the current frame back once (the same
    /// tonemapped RGBA the encoder gets, without the canvas bars of a windowed
    /// game) and hands it to the PNG thread.
    pub(super) fn screenshot_tick(&mut self) {
        let Some(shots) = &self.shots else { return };
        if !shots.take_request() {
            return;
        }
        #[cfg(feature = "encode")]
        if self.gpu.is_none() {
            let latest = self.shared.as_ref().and_then(|s| {
                s.latest_gpu.lock().ok().and_then(|f| f.as_ref().and_then(|(f, c)| Some((f.try_clone()?, *c))))
            });
            match latest.map(|(f, c)| crate::gpu_frames::read_rgba(&f).map(|(w, h, rgba)| picture(w, h, rgba, c))) {
                Some(Ok((w, h, rgba))) => shots.submit(w, h, rgba, &self.source_name),
                Some(Err(e)) => shots.fail(&format!("frame readback failed: {e}")),
                None => shots.fail("no captured frame yet"),
            }
            return;
        }
        match self.gpu.as_mut().and_then(|g| g.encode_frame()) {
            Some((w, h, rgba, content)) => {
                let (w, h, rgba) = picture(w, h, rgba, content);
                shots.submit(w, h, rgba, &self.source_name)
            }
            None => shots.fail("no captured frame yet"),
        }
    }

    /// GPU path (and before the first frame): tick every 50 ms and sleep in between. The
    /// CPU fallback switches to the redraw loop once the capture thread picks it.
    pub(super) fn idle_tick(&mut self, event_loop: &ActiveEventLoop) {
        let Some(shared) = self.shared.clone() else { return };
        if self.gpu.is_none() && shared.path.load(Relaxed) == capture::PATH_CPU {
            if let Some(window) = &self.window {
                self.gpu = Some(crate::gpu::Gpu::new(window.clone(), shared.clone()));
                window.request_redraw();
            }
        }
        if self.gpu.is_some() {
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        let now = Instant::now();
        if self.next_tick.is_none_or(|t| now >= t) {
            if shared.quit.load(Relaxed) {
                event_loop.exit();
                return;
            }
            self.screenshot_tick();
            self.tick_fps();
            self.periodic_update();
            self.next_tick = Some(now + IDLE_TICK);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_tick.unwrap_or(now + IDLE_TICK)));
    }

    pub(super) fn tick_fps(&mut self) {
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

    pub(super) fn periodic_update(&mut self) {
        let now = Instant::now();
        if self.last_periodic.is_some_and(|t| now.duration_since(t) < PERIODIC) {
            return;
        }
        self.last_periodic = Some(now);
        self.refresh_source_title();
        self.refresh_hdr_mode();
        self.restart_capture_if_needed();
        self.emit_status();
    }

    /// Starts capture on the target window (`RECALL_CAPTURE_HWND`) or the configured
    /// monitor, in the format `CaptureShared::wants_fp16` picks.
    pub(super) fn start_capture(&mut self) {
        let Some(shared) = self.shared.clone() else { return };
        let hwnd = std::env::var("RECALL_CAPTURE_HWND").ok().and_then(|v| v.parse::<usize>().ok());
        let started = match hwnd {
            Some(hwnd) => capture::start_window(hwnd, shared),
            None => capture::start_monitor(self.config.monitor, shared),
        };
        match started {
            Ok(control) => self.capture = Some(control),
            Err(e) => eprintln!("failed to start capture: {e}"),
        }
    }

    /// Restarts a Bgra8 capture in fp16 once HDR turns on or a frame needs the CPU path. A
    /// new capture brings a new D3D11 device, so the encoder reopens and the replay buffer
    /// starts again (logged).
    fn restart_capture_if_needed(&mut self) {
        let Some(shared) = self.shared.clone() else { return };
        if self.capture.is_none() || shared.fp16.load(Relaxed) || !shared.wants_fp16() {
            return;
        }
        eprintln!("[recall] capture restarts in Rgba16F (HDR display or CPU fallback); the replay buffer restarts");
        if let Some(control) = self.capture.take() {
            if let Err(e) = control.stop() {
                eprintln!("[recall] capture stop: {e}");
            }
        }
        self.start_capture();
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

    pub(super) fn refresh_hdr_mode(&mut self) {
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

    pub(super) fn emit_status(&self) {
        if !self.config.json {
            return;
        }
        let Some(shared) = &self.shared else { return };
        let (width, height) = self
            .gpu
            .as_ref()
            .and_then(|g| g.output_size())
            .or_else(|| shared.gpu_size.lock().ok().and_then(|s| *s))
            .unwrap_or((0, 0));
        let mode = if shared.mode.load(Relaxed) == capture::MODE_TONEMAP {
            "tonemap"
        } else {
            "passthrough"
        };
        let (buffer_secs, encoder_label, encoder_error, codec) = self.encoder_status();
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
            codec,
            recording_mode: "vfr".to_string(),
            mic_muted: { #[cfg(feature="encode")] { audio::mic_muted() } #[cfg(not(feature="encode"))] { false } },
        });
    }

    /// (buffer seconds, encoder label, encoder error, codec).
    #[cfg(feature = "encode")]
    fn encoder_status(&self) -> (f32, String, Option<String>, String) {
        let Some(enc) = &self.encoder else {
            return (0.0, "off".into(), None, "off".into());
        };
        let codec = enc.stats.codec().label().to_string();
        let label = match enc.stats.backend.load(Relaxed) {
            // Kept as the "replay ready" token for every codec: the overlay,
            // native app and hub compare it literally; the codec is `codec`.
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
        (elapsed_us as f32 / 1_000_000.0, label.into(), error, codec)
    }

    #[cfg(not(feature = "encode"))]
    fn encoder_status(&self) -> (f32, String, Option<String>, String) {
        (0.0, "off".into(), None, "off".into())
    }
}
