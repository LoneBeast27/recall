//! `--encoder-smoke`: real NVENC encode + the saver's mux path, synchronously,
//! without WGC or audio. Honours `RECALL_CODEC` / `RECALL_QUALITY`, so each
//! codec (and its fallback) can be checked on its own. GPU work: the caller
//! owns resource admission.

use std::time::Instant;

use ffmpeg_next as ff;

use super::replay::Replay;
use super::{open_encoder, saver, Config, EncStats, PacedFrame, SaveRequest};

/// Exercise the actual NVENC encoder and MP4 remux path without WGC or audio.
/// The caller owns GPU/resource admission and may inspect this clip with ffprobe.
pub fn smoke_test(out_dir: &std::path::Path) -> Result<String, String> {
    ff::init().map_err(|e| format!("ffmpeg init: {e}"))?;
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let cfg = Config {
        fps: 60,
        buffer_secs: 2,
        bitrate: 2_000_000,
        ..Config::from_env(false)
    };
    for note in &cfg.notes {
        eprintln!("smoke: {note}");
    }
    let (mut enc, fallback) = open_encoder(&cfg, cfg.codec, 256, 256, None)?;
    let mut replay = Replay::new(&cfg, None);
    replay.begin(&enc);
    if let Some(note) = fallback {
        eprintln!("smoke: {note}");
    }
    eprintln!(
        "smoke codec: requested {} -> recording {} ({}), quality {}",
        cfg.codec.label(),
        enc.codec.label(),
        enc.backend,
        cfg.quality.label()
    );
    let stats = EncStats::default();
    for pts in [0_u64, 16_000, 63_000, 97_000, 180_000] {
        let mut rgba = vec![0_u8; 256 * 256 * 4];
        for (i, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            pixel[0] = ((i + pts as usize) & 0xff) as u8;
            pixel[1] = (pts & 0xff) as u8;
            pixel[2] = 127;
            pixel[3] = 255;
        }
        enc.encode(
            PacedFrame {
                idx: pts,
                width: 256,
                height: 256,
                rgba,
                gpu: None,
                content: None,
            },
            &stats,
            &mut replay,
        )?;
    }
    enc.encoder
        .send_eof()
        .map_err(|e| format!("NVENC flush: {e}"))?;
    enc.drain(&stats, &mut replay)?;
    let path = out_dir.join(format!("nvenc-vfr-smoke-{}.mp4", uuid::Uuid::new_v4()));
    let path_text = path.to_string_lossy().into_owned();
    save_now(&replay, &path_text, 1, Some(220_000))?;
    // A long static tail must remain a decodable, exactly one-second still clip.
    let still_path = out_dir.join(format!("nvenc-static-smoke-{}.mp4", uuid::Uuid::new_v4()));
    save_now(&replay, &still_path.to_string_lossy(), 1, Some(300_000_000))?;
    eprintln!("static-tail smoke: {}", still_path.display());
    Ok(path_text)
}

/// Synchronous save through the saver's own build -> mux -> rename path.
fn save_now(replay: &Replay, path: &str, seconds: u32, end_us: Option<u64>) -> Result<(), String> {
    let now = Instant::now();
    let req = SaveRequest {
        path: path.to_string(),
        seconds,
        end_us,
        at_last_frame: false,
        requested: now,
    };
    saver::write_now(&replay.build_job(req, &[], now)?).map(|_| ())
}
