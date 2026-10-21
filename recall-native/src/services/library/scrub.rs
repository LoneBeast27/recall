//! Hover-scrub frames: 8 × 320 px JPEGs at (i+0.5)/8 of the clip, cached beside the
//! editor visuals (`<cache>/visuals/<source identity>/scrub-<i>.jpg`).
//!
//! Duplicates the minimal parts of `visual_assets.rs` (private to `backend`); the
//! identity hash is byte-for-byte the same so both caches share one folder per source.
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) const SCRUB_FRAMES: usize = 8;
/// CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS, as the editor's ffmpeg calls.
const HIDDEN_LOW_PRIORITY: u32 = 0x0800_4000;

pub(super) fn frames(path: &str) -> Result<Vec<String>, String> {
    frames_in(&crate::backend::cache_dir()?, path)
}

/// Returns cached frames when all eight exist; otherwise extracts the missing ones.
/// Partial success returns the frames that worked; all failing is an error.
pub(super) fn frames_in(cache_root: &Path, path: &str) -> Result<Vec<String>, String> {
    let source = std::fs::canonicalize(path).map_err(|error| format!("resolve scrub source: {error}"))?;
    let metadata = source.metadata().map_err(|error| format!("stat scrub source: {error}"))?;
    if !metadata.is_file() {
        return Err("scrub source is not a file".into());
    }
    let modified_ms = metadata.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128) as u64;
    let directory = cache_root.join("visuals").join(identity_from(&source, metadata.len(), modified_ms));
    let targets: Vec<PathBuf> = (0..SCRUB_FRAMES).map(|i| directory.join(format!("scrub-{i}.jpg"))).collect();
    let text = |path: &Path| path.to_string_lossy().into_owned();
    if targets.iter().all(|t| valid(t)) {
        return Ok(targets.iter().map(|t| text(t)).collect());
    }
    std::fs::create_dir_all(&directory).map_err(|error| format!("create scrub cache: {error}"))?;
    let duration = super::mp4_duration(&source).or_else(|| probe_duration(&source).ok()).filter(|d| d.is_finite() && *d > 0.0).ok_or("clip duration unknown")?;
    let ffmpeg = binary("RECALL_FFMPEG", "ffmpeg.exe")?;
    let mut frames = Vec::with_capacity(SCRUB_FRAMES);
    let mut errors = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        let seconds = duration * ((index as f64 + 0.5) / SCRUB_FRAMES as f64);
        match if valid(target) { Ok(()) } else { extract(&ffmpeg, &source, seconds, target) } {
            Ok(()) => frames.push(text(target)),
            Err(error) => errors.push(format!("frame {}: {error}", index + 1)),
        }
    }
    if frames.is_empty() { Err(format!("Scrub frames: {}", errors.join("; "))) } else { Ok(frames) }
}

fn extract(ffmpeg: &Path, source: &Path, seconds: f64, target: &Path) -> Result<(), String> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let temporary = target.with_file_name(format!("{}-{}-{nonce}.jpg", target.file_stem().and_then(|s| s.to_str()).unwrap_or("scrub"), std::process::id()));
    let seek = format!("{:.3}", seconds.max(0.0));
    let output = hidden(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-threads", "2", "-filter_threads", "1", "-ss", &seek, "-i"])
        .arg(source)
        .args(["-frames:v", "1", "-vf", "scale=320:-2", "-q:v", "5", "-y"])
        .arg(&temporary)
        .output()
        .map_err(|error| format!("start ffmpeg: {error}"))?;
    if !output.status.success() || !valid(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("ffmpeg: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    match std::fs::rename(&temporary, target) {
        Ok(()) => Ok(()),
        Err(_) if valid(target) => {
            let _ = std::fs::remove_file(&temporary);
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(format!("publish scrub frame: {error}"))
        }
    }
}

fn probe_duration(source: &Path) -> Result<f64, String> {
    let output = hidden(&binary("RECALL_FFPROBE", "ffprobe.exe")?)
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
        .arg(source)
        .output()
        .map_err(|error| format!("start ffprobe: {error}"))?;
    String::from_utf8_lossy(&output.stdout).trim().parse::<f64>().map_err(|_| "ffprobe returned no duration".to_string())
}

fn valid(path: &Path) -> bool {
    path.metadata().map(|m| m.is_file() && m.len() > 100).unwrap_or(false)
}

/// FNV-1a over (canonical path, bytes, mtime ms) — identical to `visual_assets::identity_from`.
pub(super) fn identity_from(source: &Path, bytes: u64, modified_ms: u64) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut feed = |data: &[u8]| {
        for byte in data {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(source.to_string_lossy().as_bytes());
    feed(&bytes.to_le_bytes());
    feed(&modified_ms.to_le_bytes());
    format!("{hash:016x}")
}

fn hidden(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(HIDDEN_LOW_PRIORITY);
    command
}

/// `RECALL_FFMPEG`/`RECALL_FFPROBE`, else `tools/ffmpeg-dev/*/bin` or `bin` above the
/// crate or the running executable (same search as `visual_assets.rs`).
pub(super) fn binary(variable: &str, executable: &str) -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os(variable) {
        let path = PathBuf::from(value);
        return if path.is_file() { Ok(path) } else { Err(format!("{variable} does not name {executable}")) };
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let running = std::env::current_exe().ok();
    let mut roots: Vec<&Path> = manifest.ancestors().collect();
    if let Some(path) = running.as_deref() {
        roots.extend(path.ancestors());
    }
    for root in roots {
        if let Ok(entries) = std::fs::read_dir(root.join("tools").join("ffmpeg-dev")) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("bin").join(executable);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        let candidate = root.join("bin").join(executable);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("{executable} not found; set {variable} to the packaged binary"))
}
