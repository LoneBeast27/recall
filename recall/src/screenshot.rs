//! Screenshot hotkey. The stdin thread only raises a flag; the render thread
//! reads back the frame the encoder sees (tonemapped SDR RGBA, `Gpu::encode_frame`)
//! once, and the `recall-screenshot` thread PNG-encodes it in memory and writes
//! `<out_dir>\screenshots\<Game>__<date>__<time>.png` in a single write (via
//! `.part` + rename). Nothing here runs on the encoder thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

use chrono::{DateTime, Local};

use crate::naming;
use crate::protocol::{emit_or_log, Outbound};

/// Frames waiting for the PNG thread (a 4K frame is 33 MB of RAM).
const QUEUE: usize = 2;

struct Shot {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    source: String,
    taken: DateTime<Local>,
}

pub struct Screenshots {
    requested: AtomicBool,
    tx: SyncSender<Shot>,
    json: bool,
}

/// Spawn the PNG writer for `out_dir`.
pub fn spawn(out_dir: String, json: bool) -> Arc<Screenshots> {
    let (tx, rx) = sync_channel::<Shot>(QUEUE);
    let spawned = std::thread::Builder::new()
        .name("recall-screenshot".into())
        .spawn(move || writer(rx, out_dir, json));
    if let Err(e) = spawned {
        emit_or_log(json, &Outbound::EncoderLog { message: format!("screenshot thread failed to start: {e}") }, || {
            format!("screenshot thread failed to start: {e}")
        });
    }
    Arc::new(Screenshots { requested: AtomicBool::new(false), tx, json })
}

impl Screenshots {
    /// Ask for the next rendered frame (stdin thread).
    pub fn request(&self) {
        self.requested.store(true, Relaxed);
    }

    /// Render thread: true once per request.
    pub fn take_request(&self) -> bool {
        self.requested.swap(false, Relaxed)
    }

    /// Render thread: hand the read-back frame to the PNG thread (never blocks).
    pub fn submit(&self, width: u32, height: u32, rgba: Vec<u8>, source: &str) {
        let shot = Shot { width, height, rgba, source: source.to_string(), taken: Local::now() };
        match self.tx.try_send(shot) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => self.fail("previous screenshots are still being written"),
            Err(TrySendError::Disconnected(_)) => self.fail("screenshot writer stopped"),
        }
    }

    pub fn fail(&self, reason: &str) {
        failed(self.json, reason);
    }
}

fn failed(json: bool, reason: &str) {
    emit_or_log(json, &Outbound::ScreenshotFailed { reason: reason.to_string() }, || format!("screenshot FAILED: {reason}"));
}

fn writer(rx: Receiver<Shot>, out_dir: String, json: bool) {
    lower_thread_priority();
    while let Ok(shot) = rx.recv() {
        match write_shot(&out_dir, &shot) {
            Ok(path) => {
                let path = path.to_string_lossy().into_owned();
                emit_or_log(json, &Outbound::Screenshot { path: path.clone() }, || format!("screenshot saved {path}"));
            }
            Err(reason) => failed(json, &reason),
        }
    }
}

fn write_shot(out_dir: &str, shot: &Shot) -> Result<PathBuf, String> {
    let png = encode_png(shot.width, shot.height, &shot.rgba)?;
    let path = naming::screenshot_path(out_dir, &shot.source, &shot.taken, Path::exists);
    let dir = path.parent().ok_or("screenshot path has no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let part = naming::part_of(&path);
    std::fs::write(&part, &png).map_err(|e| format!("write {}: {e}", part.display()))?;
    std::fs::rename(&part, &path).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        format!("finalize {}: {e}", path.display())
    })?;
    Ok(path)
}

/// Tightly packed RGBA8 -> 8-bit RGB PNG (alpha dropped: the capture's alpha
/// is not meaningful, NVENC ignores it too). Fast deflate: a 4K frame takes
/// a fraction of a second on the background thread.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let pixels = width as usize * height as usize;
    if width == 0 || height == 0 || rgba.len() < pixels * 4 {
        return Err(format!("frame {width}x{height} has {} bytes", rgba.len()));
    }
    let mut rgb = Vec::with_capacity(pixels * 3);
    for px in rgba[..pixels * 4].chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
    }
    let mut out = Vec::with_capacity(pixels);
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut w = encoder.write_header().map_err(|e| format!("png header: {e}"))?;
        w.write_image_data(&rgb).map_err(|e| format!("png encode: {e}"))?;
    }
    Ok(out)
}

/// Keep PNG work below the render and encoder threads.
fn lower_thread_priority() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    // SAFETY: the current-thread pseudo-handle needs no closing and is used only here.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trips_rgb_and_drops_alpha() {
        let (w, h) = (3u32, 2u32);
        let rgba: Vec<u8> = (0..w * h).flat_map(|i| [i as u8 * 10, 100, 200, 0]).collect();
        let png = encode_png(w, h, &rgba).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height, info.color_type), (3, 2, png::ColorType::Rgb));
        assert_eq!(&buf[..6], &[0, 100, 200, 10, 100, 200]);
        assert_eq!(&buf[15..18], &[50, 100, 200]);
    }

    #[test]
    fn short_frames_are_rejected() {
        assert!(encode_png(4, 4, &[0; 10]).is_err());
        assert!(encode_png(0, 4, &[]).is_err());
    }

    #[test]
    fn a_shot_lands_in_the_screenshots_folder_without_a_part_file() {
        let dir = std::env::temp_dir().join(format!("recall-shot-{}", std::process::id()));
        let shot = Shot { width: 2, height: 2, rgba: vec![255; 16], source: "Elden Ring".into(), taken: Local::now() };
        let path = write_shot(&dir.to_string_lossy(), &shot).unwrap();
        assert_eq!(path.parent().unwrap(), dir.join("screenshots"));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("Elden-Ring__") && name.ends_with(".png"), "{name}");
        assert!(path.is_file() && !naming::part_of(&path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn requests_are_taken_once() {
        let shots = spawn(std::env::temp_dir().to_string_lossy().into_owned(), true);
        assert!(!shots.take_request());
        shots.request();
        shots.request();
        assert!(shots.take_request());
        assert!(!shots.take_request());
    }
}
