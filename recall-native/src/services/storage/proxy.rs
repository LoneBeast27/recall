//! Preview copies for clips Media Foundation will not play (AV1 through MFPlay fails with
//! E_NOTIMPL even with the AV1 Video Extension installed): a 720p H.264 video-only file in
//! `<cache>\proxy`, frame timestamps kept so seeking lines up with the original. Export and
//! audio always use the original. At most [`KEEP`] copies stay on the SSD.
use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    time::UNIX_EPOCH,
};

use super::Priority;

const KEEP: usize = 3;

/// Encoder settings tried in order: NVENC (light, GPU), then x264 on the CPU.
const ENCODERS: [&[&str]; 2] = [
    &["-c:v", "h264_nvenc", "-preset", "p1", "-cq", "30"],
    &["-c:v", "libx264", "-preset", "veryfast", "-crf", "26", "-threads", "8"],
];

/// Returns the preview copy for `source`, making it first if needed (blocking; call from a
/// worker thread). A clip on a hard drive is read through that drive's lane.
pub fn preview_proxy(source: &str) -> Result<String, String> {
    // A light copy (`light.rs`) is a sharper preview copy already on the SSD.
    if let Some(light) = super::light::light_copy(source) {
        return Ok(light);
    }
    let src = Path::new(source);
    let meta = src.metadata().map_err(|e| format!("read clip: {e}"))?;
    let dir = super::cache_root().join("proxy");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create preview folder: {e}"))?;
    let target = dir.join(format!("{:016x}.mp4", identity(source, &meta)));
    if target.is_file() {
        return Ok(target.to_string_lossy().into_owned());
    }
    let tmp = target.with_extension("part");
    let ffmpeg = super::binary("RECALL_FFMPEG", "ffmpeg.exe")?;
    let lowered = super::drive_kind(src).is_hdd();
    let mut last_error = String::new();
    for encoder in ENCODERS {
        let mut command = Command::new(&ffmpeg);
        command
            // Decode on the GPU where it can (AV1 on RTX 30 and newer); FFmpeg decodes on the CPU
            // where it cannot. FFmpeg 7.1 has no D3D11 scaler, so frames come back for the scale.
            .args(["-hide_banner", "-loglevel", "error", "-y", "-hwaccel", "d3d11va", "-i"])
            .arg(src)
            .args(["-map", "0:v:0", "-an", "-sn", "-vf", "scale=-2:'min(720,ih)'", "-fps_mode", "passthrough"])
            .args(encoder)
            .args(["-f", "mp4", "-movflags", "+faststart"])
            .arg(&tmp);
        let output = super::run_io(src, Priority::Interactive, move || super::run_hidden(&mut command, lowered))?
            .map_err(|e| format!("start ffmpeg: {e}"))?;
        if output.status.success() && tmp.is_file() {
            std::fs::rename(&tmp, &target).map_err(|e| format!("save preview copy: {e}"))?;
            prune(&dir);
            return Ok(target.to_string_lossy().into_owned());
        }
        last_error = String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or("ffmpeg failed").to_owned();
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last_error)
}

/// Removes every preview copy (Settings → Clear). Returns bytes freed.
pub fn clear() -> u64 {
    let dir = super::cache_root().join("proxy");
    let freed = copies(&dir).iter().map(|(_, bytes, _)| bytes).sum();
    let _ = std::fs::remove_dir_all(&dir);
    freed
}

/// Same clip, size and modification time → same copy.
fn identity(source: &str, meta: &std::fs::Metadata) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.to_lowercase().hash(&mut hasher);
    meta.len().hash(&mut hasher);
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis()).hash(&mut hasher);
    hasher.finish()
}

/// (path, bytes, modified) of each finished copy.
fn copies(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let Ok(listing) = std::fs::read_dir(dir) else { return Vec::new() };
    listing
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "mp4"))
        .filter_map(|e| e.metadata().ok().map(|m| (e.path(), m.len(), m.modified().unwrap_or(UNIX_EPOCH))))
        .collect()
}

/// Keeps the newest [`KEEP`] copies.
fn prune(dir: &Path) {
    let mut all = copies(dir);
    all.sort_by(|a, b| b.2.cmp(&a.2));
    for (path, _, _) in all.into_iter().skip(KEEP) {
        let _ = std::fs::remove_file(path);
    }
}
