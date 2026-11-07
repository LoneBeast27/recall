//! The encoder thread's loop: frames in, packets into the replay buffer, saves out.
//!
//! The replay buffer (`replay.rs`) is owned here, not by the encoder: a fatal encode error
//! or a failed open drops the encoder only, and `reopen.rs` decides when it opens again.
//! Saves in the gap and after use the frames already buffered.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ffmpeg_next as ff;

use super::frame_fit::{normalize_small_size_jitter, SmallFrames};
use super::handoff::handle_save;
use super::reopen::{self, GpuProbe, Input, Reopen};
use super::replay::Replay;
use super::saver::SaverQueue;
use super::settings::Codec;
use super::{gpu_pool_of, open_checked, report, vram_watch, Config, EncStats, Enc, PacedFrame, SaveRequest};
use crate::audio::TrackRing;
use crate::spill::Spill;

pub(super) fn run(
    cfg: Config,
    frames: Receiver<PacedFrame>,
    commands: Receiver<SaveRequest>,
    stats: Arc<EncStats>,
    audio: Vec<Arc<TrackRing>>,
    saver: SaverQueue,
) {
    if let Err(e) = ff::init() {
        let reason = format!("ffmpeg init failed: {e}");
        report(cfg.json, &reason);
        stats.backend.store(3, Relaxed);
        *stats.error.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason);
        return;
    }
    report(cfg.json, &cfg.summary());
    for note in &cfg.notes {
        report(cfg.json, note);
    }
    let mut live = Live::new(&cfg, &stats, audio.len());
    loop {
        // One beat per turn, frames or not: a static game keeps it, a hung NVENC call
        // (inside `live.frame`) stops it (`EncStats::heartbeat`).
        stats.heartbeat.fetch_add(1, Relaxed);
        // Saves first: they're rare and user-facing latency matters (R10).
        while let Ok(req) = commands.try_recv() {
            handle_save(live.replay.as_ref(), req, &stats, &audio, &saver);
        }
        match frames.recv_timeout(Duration::from_millis(100)) {
            Ok(frame) => live.frame(frame, &cfg, &stats),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        live.tick(Instant::now(), &cfg, &stats);
    }
    // Quit: saves requested before the frame channel closed still get written
    // (the saver drains them within the shutdown budget).
    while let Ok(req) = commands.try_recv() {
        handle_save(live.replay.as_ref(), req, &stats, &audio, &saver);
    }
}

struct Live {
    enc: Option<Enc>,
    replay: Option<Replay>,
    /// A failed encoder opens again on a backoff, never left off for good.
    reopen: Reopen,
    /// GPU frames refused: when to try them again.
    probe: GpuProbe,
    vram: vram_watch::VramWatch,
    small: SmallFrames,
    /// After one fallback the engine stays on H.264: reopening on a resolution
    /// change must not flap codecs or re-log the same failure.
    want: Codec,
    /// One RAM budget and spill file for the whole run; every ring shares it.
    spill: Arc<Spill>,
}

impl Live {
    fn new(cfg: &Config, stats: &EncStats, tracks: usize) -> Self {
        let json = cfg.json;
        let spill = Arc::new(Spill::from_system(cfg.buffer_secs, cfg.bitrate, tracks, Arc::new(move |m: &str| report(json, m))));
        Self {
            enc: None,
            replay: None,
            reopen: Reopen::with_delays(super::retry_delays(stats)),
            probe: GpuProbe::with_delays(super::probe_delays(stats), super::probe_verdict(stats)),
            vram: Default::default(),
            small: SmallFrames::default(),
            want: cfg.codec,
            spill,
        }
    }

    fn frame(&mut self, mut frame: PacedFrame, cfg: &Config, stats: &EncStats) {
        // After a refusal, GPU frames still queued are dropped (capture has switched).
        if frame.gpu.is_some() && stats.gpu_refused.load(Relaxed) {
            return;
        }
        if self.small.skips(&frame, |note| report(cfg.json, note)) {
            return;
        }
        // GPU frames arrive already cropped / padded to a stable size.
        if let Some(active) = self.enc.as_ref().filter(|_| frame.gpu.is_none()) {
            if let Err(e) = normalize_small_size_jitter(&mut frame, active.width, active.height) {
                report(cfg.json, &format!("capture frame rejected: {e}"));
                return;
            }
        }
        let size = (frame.width & !1, frame.height & !1);
        if self.replay.as_ref().and_then(Replay::size).is_some_and(|s| s != size) {
            report(cfg.json, "capture resolution changed; reopening NVENC and resetting replay buffer");
            self.enc = None;
            self.replay = None;
            stats.clear_buffer();
            crate::buffer_notice::send(cfg.json, crate::buffer_notice::encoder_input(true));
        }
        if self.enc.as_ref().is_some_and(|e| e.gpu_pool != gpu_pool_of(&frame)) {
            report(cfg.json, "capture input changed (GPU / CPU frames); reopening NVENC, the replay buffer is kept");
            self.enc = None;
            crate::buffer_notice::send(cfg.json, crate::buffer_notice::encoder_input(false));
        }
        let input = Input { width: frame.width, height: frame.height, pool: gpu_pool_of(&frame) };
        if self.enc.is_none() && self.reopen.may_open(input, Instant::now()) {
            self.open(&frame, input, cfg, stats);
        }
        let (Some(enc), Some(replay)) = (self.enc.as_mut(), self.replay.as_mut()) else { return };
        let idx = frame.idx;
        match enc.encode(frame, stats, replay) {
            Ok(()) => self.reopen.encoded(Instant::now()),
            Err(reason) => {
                report(cfg.json, &format!("encode error at frame {idx}: {reason}"));
                crate::longrec::tap::on_encode_error(); // long-recording hook: a freeze starts here
                let reason = format!("NVENC encode failed: {reason}");
                // The long recording waits: it ends only if the recovery fails (`tick`).
                self.enc = None;
                reopen::encode_failed(&mut self.reopen, input, stats, cfg.json);
                stats.backend.store(3, Relaxed);
                *stats.error.lock().unwrap_or_else(|p| p.into_inner()) = Some(reason);
            }
        }
    }

    fn open(&mut self, frame: &PacedFrame, input: Input, cfg: &Config, stats: &EncStats) {
        // Flagged while it runs (`status.encoder_opening`): a TDR open can take ~10 s.
        stats.opening.store(true, Relaxed);
        let opened = open_checked(cfg, self.want, frame, stats);
        stats.opening.store(false, Relaxed);
        match opened {
            Ok((e, fallback)) => {
                crate::longrec::tap::on_encoder_open(&e.encoder); // long-recording hook
                if let Some(note) = fallback {
                    report(cfg.json, &note);
                    self.want = e.codec;
                }
                stats.codec.store(e.codec.index(), Relaxed);
                stats.backend.store(1, Relaxed);
                *stats.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                report(cfg.json, &format!("encoder up: {} {}x{}", e.backend, e.width, e.height));
                let _ = crate::gpu_frames::adapter::vram_line("encoder open").inspect(|l| report(cfg.json, l));
                self.vram.opened();
                self.reopen.opened(Instant::now());
                if frame.gpu.is_some() {
                    self.probe.gpu_ok();
                }
                self.replay.get_or_insert_with(|| Replay::new(cfg, Some(Arc::clone(&self.spill)))).begin(&e);
                self.enc = Some(e);
            }
            Err(e) => {
                stats.backend.store(3, Relaxed);
                *stats.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.clone());
                report(cfg.json, &format!("encoder init failed: {e}"));
                if reopen::open_failed(&mut self.reopen, input, frame.gpu.is_some(), stats, cfg.json) {
                    self.probe.refused(Instant::now());
                }
            }
        }
    }

    /// Every loop turn, frames or not: the status, the GPU retry.
    fn tick(&mut self, now: Instant, cfg: &Config, stats: &EncStats) {
        if self.enc.is_none() {
            // No frame to retry with is no failure: a static game waits here as long as it
            // takes (`reopen.rs`); only real open or encode errors escalate.
            if let Some(backend) = self.reopen.status() {
                stats.backend.store(backend, Relaxed);
                if backend == 3 {
                    // Recovery spent (status `failed`): a running long recording ends here.
                    let reason = stats.error.lock().unwrap_or_else(|p| p.into_inner()).clone();
                    crate::longrec::tap::on_encoder_failed(reason.as_deref().unwrap_or("the encoder could not reopen")); // long-recording hook
                }
            }
        }
        // A try nobody answered (no GPU frame came: capture's pipeline did not start again)
        // is a refusal: CPU copies again, and the later tries stay scheduled.
        if self.probe.unanswered(now) && !stats.gpu_refused.load(Relaxed) {
            report(cfg.json, "the GPU path did not come back; recording from CPU copies again");
            stats.gpu_retry.store(false, Relaxed);
            stats.gpu_refused.store(true, Relaxed);
            self.probe.refused(now);
        }
        if stats.gpu_refused.load(Relaxed) && self.probe.due(now) {
            report(cfg.json, "trying the GPU path again (zero-copy)");
            stats.gpu_refused.store(false, Relaxed);
            stats.gpu_retry.store(true, Relaxed);
        }
        let _ = self.vram.due().inspect(|l| report(cfg.json, l));
    }
}
