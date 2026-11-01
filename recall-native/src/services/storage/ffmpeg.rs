//! ffmpeg / ffprobe helpers shared by the thumbnail tier, scrub frames and editor visuals.
//! Every helper runs hidden at below-normal CPU priority; `lowered` additionally starts it
//! at very low I/O priority (use it whenever the input is on an HDD).
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::io_priority;

/// How a frame is located: `Fast` takes the keyframe at or before the time (one GOP read at
/// most, ~1 s accuracy on recorder clips); `Exact` decodes up to the requested time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seek {
    Fast,
    Exact,
}

/// Runs `command` hidden (stdin null, stdout/stderr captured) and waits for it.
pub fn run_hidden(command: &mut Command, lowered: bool) -> io::Result<Output> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    io_priority::output(command, lowered)
}

/// How a [`run_cancellable`] helper ended.
pub enum Run {
    Finished(Output),
    /// Stopped by the flag, or never started (`pid` 0). The process is gone.
    Cancelled { pid: u32 },
}

/// [`run_hidden`] that ends the helper as soon as `cancel` turns true.
pub fn run_cancellable(command: &mut Command, lowered: bool, cancel: &AtomicBool) -> io::Result<Run> {
    if cancel.load(Ordering::Acquire) {
        return Ok(Run::Cancelled { pid: 0 });
    }
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = io_priority::spawn(command, lowered)?;
    // Both pipes drain on their own threads: a helper that fills one would otherwise never exit.
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    };
    let stdout = child.stdout.take().map(|pipe| drain(Box::new(pipe)));
    let stderr = child.stderr.take().map(|pipe| drain(Box::new(pipe)));
    let collect = |thread: Option<std::thread::JoinHandle<Vec<u8>>>| thread.and_then(|t| t.join().ok()).unwrap_or_default();
    loop {
        if cancel.load(Ordering::Acquire) {
            let pid = child.id();
            let _ = child.kill();
            child.wait()?;
            return Ok(Run::Cancelled { pid });
        }
        if let Some(status) = child.try_wait()? {
            return Ok(Run::Finished(Output { status, stdout: collect(stdout), stderr: collect(stderr) }));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `variable`, else `bin/<executable>` or `tools/ffmpeg-dev/*/bin/<executable>` above the
/// running executable (an installed copy uses its own `bin`), then above the crate.
pub fn binary(variable: &str, executable: &str) -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os(variable) {
        let path = PathBuf::from(value);
        return if path.is_file() { Ok(path) } else { Err(format!("{variable} does not name {executable}")) };
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let running = std::env::current_exe().ok();
    let mut roots: Vec<&Path> = running.as_deref().map(|p| p.ancestors().collect()).unwrap_or_default();
    roots.extend(manifest.ancestors());
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

pub(crate) fn ffmpeg() -> Result<PathBuf, String> {
    binary("RECALL_FFMPEG", "ffmpeg.exe")
}

/// One JPEG frame of `source` at `seconds`, scaled to `width` px, published atomically at
/// `target` (an existing valid target wins a race).
pub(crate) fn extract_frame(source: &Path, seconds: f64, width: u32, quality: u8, target: &Path, seek: Seek, lowered: bool) -> Result<(), String> {
    let temporary = super::temp_sibling(target).with_extension("jpg");
    let time = format!("{:.3}", seconds.max(0.0));
    let mut command = Command::new(ffmpeg()?);
    command.args(["-hide_banner", "-loglevel", "error", "-threads", "2", "-filter_threads", "1"]);
    if seek == Seek::Fast {
        command.arg("-noaccurate_seek");
    }
    command
        .args(["-ss", &time, "-i"])
        .arg(source)
        .args(["-frames:v", "1", "-vf", &format!("scale={width}:-2"), "-q:v", &quality.to_string(), "-y"])
        .arg(&temporary);
    let output = run_hidden(&mut command, lowered).map_err(|error| format!("start ffmpeg: {error}"))?;
    if !output.status.success() || !super::valid_file(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("ffmpeg frame: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    publish(&temporary, target)
}

/// Re-encodes an image (already on the SSD) at `width` px.
pub(crate) fn scale_image(source: &Path, width: u32, target: &Path) -> Result<(), String> {
    let temporary = super::temp_sibling(target).with_extension("jpg");
    let mut command = Command::new(ffmpeg()?);
    command
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(source)
        .args(["-vf", &format!("scale={width}:-2"), "-q:v", "4", "-y"])
        .arg(&temporary);
    let output = run_hidden(&mut command, false).map_err(|error| format!("start ffmpeg: {error}"))?;
    if !output.status.success() || !super::valid_file(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("ffmpeg scale: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    publish(&temporary, target)
}

/// Container duration via ffprobe (fallback when the MP4 box parser cannot answer).
pub(crate) fn probe_duration(source: &Path, lowered: bool) -> Result<f64, String> {
    let mut command = Command::new(binary("RECALL_FFPROBE", "ffprobe.exe")?);
    command.args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"]).arg(source);
    let output = run_hidden(&mut command, lowered).map_err(|error| format!("start ffprobe: {error}"))?;
    if !output.status.success() {
        return Err(format!("ffprobe duration: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| "ffprobe returned no usable duration".into())
}

/// Atomic rename; when another writer published first, its file is kept.
pub(crate) fn publish(temporary: &Path, target: &Path) -> Result<(), String> {
    match std::fs::rename(temporary, target) {
        Ok(()) => Ok(()),
        Err(_) if super::valid_file(target) => {
            let _ = std::fs::remove_file(temporary);
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_file(temporary);
            Err(format!("publish cache file: {error}"))
        }
    }
}
