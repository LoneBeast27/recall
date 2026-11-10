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

/// `status.audio`: each source's current capture.
#[cfg(feature = "encode")]
fn audio_opened() -> Vec<protocol::AudioOpened> {
    audio::sources::opened()
        .into_iter()
        .map(|o| protocol::AudioOpened { id: o.id, name: o.name, opened: o.opened, live: o.live, adopt_id: o.adopt_id, held: o.held, mutes: o.mutes, retired: o.retired })
        .collect()
}

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
        self.watch_capture();
        self.emit_status();
    }

    /// Starts capture on the target window (`RECALL_CAPTURE_HWND`) or the configured
    /// monitor, in the format `CaptureShared::wants_fp16` picks.
    pub(super) fn start_capture(&mut self) {
        if self.shared.is_none() {
            return;
        }
        let result = self.try_start_capture();
        crate::capture_watchdog::note_start(&mut self.capture_watch, result, Instant::now());
    }

    /// `start_capture` without telling the watchdog (`capture_watchdog::watch` does).
    pub(super) fn try_start_capture(&mut self) -> Result<(), String> {
        let Some(shared) = self.shared.clone() else { return Err("no capture state".into()) };
        let hwnd = std::env::var("RECALL_CAPTURE_HWND").ok().and_then(|v| v.parse::<usize>().ok());
        let started = match hwnd {
            Some(hwnd) => capture::start_window(hwnd, shared),
            None => capture::start_monitor(self.config.monitor, shared),
        };
        started.map(|control| self.capture = Some(control)).map_err(|e| e.to_string())
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
            encoder_heartbeat: self.encoder_heartbeat(),
            encoder_opening: self.encoder_opening(),
            save_writing: self.save_writing(),
            mic_muted: { #[cfg(feature="encode")] { audio::mic_muted() } #[cfg(not(feature="encode"))] { false } },
            stdin_ended: crate::engine_stdin::stdin_ended(),
            audio: { #[cfg(feature="encode")] { audio_opened() } #[cfg(not(feature="encode"))] { Vec::new() } },
        });
    }

    /// (buffer seconds, encoder label, encoder error, codec).
    #[cfg(feature = "encode")]
    fn encoder_status(&self) -> (f32, String, Option<String>, String) {
        let Some(enc) = &self.encoder else {
            return (0.0, "off".into(), None, "off".into());
        };
        let codec = enc.stats.codec().label().to_string();
        // "h264_nvenc" stays the "replay ready" token for every codec (`EncStats::label`).
        let label = enc.stats.label();
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

    /// `status.encoder_heartbeat` (`EncStats::heartbeat`; 0 without an encoder).
    fn encoder_heartbeat(&self) -> u64 {
        #[cfg(feature = "encode")]
        if let Some(enc) = &self.encoder {
            return enc.stats.heartbeat.load(Relaxed);
        }
        0
    }

    /// `status.save_writing` (`EncoderHandle::save_writing`; none without an encoder).
    fn save_writing(&self) -> Option<protocol::SaveWriting> {
        #[cfg(feature = "encode")]
        if let Some(enc) = &self.encoder {
            return enc.save_writing();
        }
        None
    }

    /// `status.encoder_opening` (`EncStats::opening`).
    fn encoder_opening(&self) -> bool {
        #[cfg(feature = "encode")]
        if let Some(enc) = &self.encoder {
            return enc.stats.opening.load(Relaxed);
        }
        false
    }

    #[cfg(not(feature = "encode"))]
    fn encoder_status(&self) -> (f32, String, Option<String>, String) {
        (0.0, "off".into(), None, "off".into())
    }
}
