//! ffmpeg / ffprobe helpers shared by the thumbnail tier, scrub frames and editor visuals.
//! Every helper runs hidden at below-normal CPU priority; `lowered` additionally starts it
//! at very low I/O priority (use it whenever the input is on an HDD).
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
    /// Ran past its limit. The process is gone.
    TimedOut { pid: u32 },
}

/// [`run_hidden`] that ends the helper as soon as `cancel` turns true, or at `limit`.
pub fn run_cancellable(command: &mut Command, lowered: bool, cancel: &AtomicBool, limit: Duration) -> io::Result<Run> {
    run_cancellable_lines(command, lowered, cancel, limit, None)
}

/// Each stdout line as the helper prints it (progress reports).
pub type OnLine = Box<dyn FnMut(&str) + Send>;

/// [`run_cancellable`] that also hands every stdout line to `on_line` as it arrives.
pub fn run_cancellable_lines(command: &mut Command, lowered: bool, cancel: &AtomicBool, limit: Duration, on_line: Option<OnLine>) -> io::Result<Run> {
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
    let stdout = child.stdout.take().map(|pipe| match on_line {
        Some(on_line) => drain_lines(pipe, on_line),
        None => drain(Box::new(pipe)),
    });
    let stderr = child.stderr.take().map(|pipe| drain(Box::new(pipe)));
    wait_child(child, stdout, stderr, cancel, limit)
}

type Drained = Option<std::thread::JoinHandle<Vec<u8>>>;

/// Waits for `child`, killing it at `limit` or once `cancel` turns true. A helper that already
/// exited is `Finished` even when cancel came at the same moment: its work (a published
/// export) is done, and reporting it as cancelled would hide it.
fn wait_child(mut child: std::process::Child, stdout: Drained, stderr: Drained, cancel: &AtomicBool, limit: Duration) -> io::Result<Run> {
    let collect = |thread: Drained| thread.and_then(|t| t.join().ok()).unwrap_or_default();
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Run::Finished(Output { status, stdout: collect(stdout), stderr: collect(stderr) }));
        }
        let late = started.elapsed() >= limit;
        if late || cancel.load(Ordering::Acquire) {
            let pid = child.id();
            let _ = child.kill();
            child.wait()?;
            return Ok(if late { Run::TimedOut { pid } } else { Run::Cancelled { pid } });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Collects `pipe` like the plain drain, calling `on_line` for each complete line on the way.
fn drain_lines(pipe: impl Read + Send + 'static, mut on_line: OnLine) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(pipe);
        let mut bytes = Vec::new();
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line).is_ok_and(|read| read > 0) {
            on_line(String::from_utf8_lossy(&line).trim_end());
            bytes.append(&mut line);
        }
        bytes
    })
}

/// [`run_hidden`] with an end: the process is killed once it has run for `limit`, or as soon as
/// `stop` says so. A process that never returns cannot hold a thread for good.
pub fn run_watched(command: &mut Command, lowered: bool, limit: Duration, stop: &dyn Fn() -> bool) -> io::Result<Output> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    watch(io_priority::spawn(command, lowered)?, limit, stop)
}

/// [`run_watched`] for a long background encode ([`io_priority::spawn_pinned`]).
pub(super) fn run_pinned(command: &mut Command, limit: Duration, stop: &dyn Fn() -> bool) -> io::Result<Output> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    watch(io_priority::spawn_pinned(command)?, limit, stop)
}

fn watch(mut child: std::process::Child, limit: Duration, stop: &dyn Fn() -> bool) -> io::Result<Output> {
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(io::Error::other("helper process has no pipes"));
    };
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Ok(status);
        }
        let late = started.elapsed() >= limit;
        if late || stop() {
            let _ = child.kill();
            let _ = child.wait();
            break Err(io::Error::new(if late { io::ErrorKind::TimedOut } else { io::ErrorKind::Interrupted }, if late { "took too long" } else { "no longer needed" }));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (stdout, stderr) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    Ok(Output { status: status?, stdout, stderr })
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
        // Installed: ffmpeg.exe sits next to the app and shares its FFmpeg DLLs (one copy).
        for candidate in [root.join(executable), root.join("bin").join(executable)] {
            if candidate.is_file() {
                return Ok(candidate);
            }
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

/// Longest a length probe may run: a hung ffprobe must not hold its disk's lane for good.
const PROBE_LIMIT: Duration = Duration::from_secs(15);

/// Container duration via ffprobe (fallback when the MP4 box parser cannot answer).
pub(crate) fn probe_duration(source: &Path, lowered: bool) -> Result<f64, String> {
    let mut command = Command::new(binary("RECALL_FFPROBE", "ffprobe.exe")?);
    command.args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"]).arg(source);
    probe_duration_with(&mut command, lowered, PROBE_LIMIT)
}

/// [`probe_duration`] for a built command, killed after `limit`.
fn probe_duration_with(command: &mut Command, lowered: bool, limit: Duration) -> Result<f64, String> {
    let output = run_watched(command, lowered, limit, &|| false).map_err(|error| format!("ffprobe duration: {error}"))?;
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

/// The first video stream's frame count from the container header (no decode; an MP4 keeps
/// it in its sample table). `None` when the container does not say.
pub(crate) fn probe_frames(source: &Path, lowered: bool) -> Option<u64> {
    let mut command = Command::new(binary("RECALL_FFPROBE", "ffprobe.exe").ok()?);
    command.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=nb_frames", "-of", "default=nw=1:nk=1"]).arg(source);
    let output = run_hidden(&mut command, lowered).ok()?;
    output.status.success().then_some(())?;
    String::from_utf8_lossy(&output.stdout).trim().parse::<u64>().ok().filter(|&frames| frames > 0)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Cancel pressed just as the helper finished (the engine had published its export): the
    /// run is reported as finished, not cancelled.
    #[test]
    fn a_helper_that_already_exited_is_finished_even_when_cancel_came_too() {
        let mut command = Command::new("cmd");
        command.args(["/c", "exit 0"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = io_priority::spawn(&mut command, false).unwrap();
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            // Poll the OS handle without reaping, until the helper is gone.
            let gone = unsafe { windows::Win32::System::Threading::WaitForSingleObject(windows::Win32::Foundation::HANDLE(std::os::windows::io::AsRawHandle::as_raw_handle(&child)), 0) } == windows::Win32::Foundation::WAIT_OBJECT_0;
            if gone {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let drain = |pipe: Option<std::process::ChildStdout>| pipe.map(|mut p| std::thread::spawn(move || { let mut b = Vec::new(); let _ = p.read_to_end(&mut b); b }));
        let drain_err = |pipe: Option<std::process::ChildStderr>| pipe.map(|mut p| std::thread::spawn(move || { let mut b = Vec::new(); let _ = p.read_to_end(&mut b); b }));
        let cancel = AtomicBool::new(true);
        let run = wait_child(child, drain(stdout), drain_err(stderr), &cancel, Duration::from_secs(10)).unwrap();
        assert!(matches!(run, Run::Finished(ref output) if output.status.success()), "cancelled after it finished");
    }

    fn slow_process() -> Command {
        let mut command = Command::new("ping.exe");
        command.args(["-n", "30", "127.0.0.1"]);
        command
    }

    #[test]
    fn a_process_that_never_returns_is_killed_at_its_limit() {
        let started = Instant::now();
        let error = run_watched(&mut slow_process(), false, Duration::from_millis(300), &|| false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_length_probe_that_hangs_is_stopped() {
        let started = Instant::now();
        let error = probe_duration_with(&mut slow_process(), false, Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("took too long"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_process_nobody_needs_any_more_is_killed() {
        let error = run_watched(&mut slow_process(), false, Duration::from_secs(60), &|| true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn a_process_that_finishes_gives_its_output() {
        let mut command = Command::new("cmd.exe");
        command.args(["/c", "echo hello"]);
        let output = run_watched(&mut command, false, Duration::from_secs(20), &|| false).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }
}
