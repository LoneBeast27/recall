//! Light copies: what the SSD keeps of a clip instead of the whole file on a small cache (the
//! 2 GiB default on a full system drive). `light/<id>.mp4` is the clip's video at its own
//! resolution in HEVC at ~10 Mbit/s: measured on a 2:00 1440p60 VALORANT clip (2026-11-03),
//! 158 MiB instead of 400 MB, crosshair dot and HUD text kept, 26 s of NVENC. No audio: the
//! editor's sound is its stems, and export always reads the original.
//!
//! Made at idle only (the planner's [`super::prefetch`] light step, and right after a fresh
//! clip is adopted from the recorder's SSD copy, `kept.rs`), never while the recorder works.
//! The encoder is a hidden ffmpeg at below-normal priority, very low I/O priority, pinned to
//! the upper half of the CPUs, decoding on the GPU through D3D11 into NVENC without a trip
//! through system memory. Reading a hard drive, it stops the moment
//! on-demand work queues on that drive's lane; the item is tried again later.
//!
//! Units count in the video tier as `light/<id>.mp4` (`budget.rs`): score-ranked and evicted
//! like full copies.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Target and peak video rate of a light copy.
pub const LIGHT_RATE: &str = "10M";
pub const LIGHT_PEAK: &str = "12500k";
/// Planning estimate: bytes a second of clip (10 Mbit/s plus container overhead).
pub const LIGHT_BYTES_PER_SEC: u64 = 1_350_000;
/// An encode that runs this many times the clip's length is given up.
const LIMIT_FACTOR: u64 = 4;

/// Why a light copy was not made.
pub(super) const STOPPED: &str = "the light copy gave the drive to on-demand work";

pub(super) fn light_dir(root: &Path) -> PathBuf {
    root.join("light")
}

/// `light/<id>.mp4` of the clip at `source`, if it is there (stats the source, no data read).
pub fn light_copy(source: &str) -> Option<String> {
    light_in(&super::cache_root(), Path::new(source))
}

pub(super) fn light_in(root: &Path, source: &Path) -> Option<String> {
    let (id, _, _) = super::identity_of(source).ok()?;
    let path = light_dir(root).join(format!("{id}.mp4"));
    super::valid_file(&path).then(|| path.to_string_lossy().into_owned())
}

/// The encoder's arguments: `input` to `output`, video only, timestamps kept (the editor swaps
/// between this and the original at the same times). `gpu`: decoded frames stay in VRAM and
/// go straight to NVENC; else they come back to system memory first.
pub(super) fn arguments(input: &Path, output: &Path, gpu: bool) -> Vec<String> {
    // Decode on the GPU through D3D11: NVDEC (`-hwaccel cuda`) rejects recorder clips here
    // (cuvidCreateDecoder: CUDA_ERROR_INVALID_VALUE on 1440p60 H.264 Main, FFmpeg 7.1.1 on an
    // RTX 5090) and FFmpeg fell back to the CPU: 74 s of CPU for a 2:00 clip.
    // GPU-only (`-hwaccel_output_format d3d11`): 4.1 CPU-s for the 2:00 1440p60 VALORANT clip
    // against 33.6 when frames came back to system memory, same 7599 frames (2026-11-04). Without
    // `-extra_hw_frames` the decoder's pool ran out of surfaces while NVENC held frames and the
    // copy ended silently early, so [`make_in`] also checks the frame count. Where D3D11 decode
    // is not there, FFmpeg decodes on at most 4 CPU threads.
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-y", "-hwaccel", "d3d11va"].map(String::from).to_vec();
    if gpu {
        args.extend(["-hwaccel_output_format", "d3d11", "-extra_hw_frames", "16"].map(String::from));
    }
    args.extend(["-threads", "4", "-i"].map(String::from));
    args.push(input.to_string_lossy().into_owned());
    args.extend(
        [
            "-map", "0:v:0", "-an", "-sn", "-dn", "-fps_mode", "passthrough", "-c:v", "hevc_nvenc", "-preset", "p4", "-tune", "hq", "-rc", "vbr", "-multipass", "qres",
            "-b:v", LIGHT_RATE, "-maxrate", LIGHT_PEAK, "-bufsize", "20M", "-tag:v", "hvc1", "-movflags", "+faststart", "-f", "mp4",
        ]
        .map(String::from),
    );
    args.push(output.to_string_lossy().into_owned());
    args
}

/// True when `made` holds every frame of the source (`source_frames`; unknown passes).
pub(super) fn complete(source_frames: Option<u64>, made: Option<u64>) -> bool {
    match (source_frames, made) {
        (Some(want), Some(got)) => got == want,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

/// Makes the light copy of clip `id` from `read_from` (the original, or an SSD copy of it) for
/// as long as `stop` says no. Ok(path) when made (or already there). The GPU-only encode runs
/// first; when it fails or comes out short, the copy is made again through system memory.
pub(super) fn make_in(root: &Path, id: &str, read_from: &Path, seconds: f64, stop: &dyn Fn() -> bool) -> Result<PathBuf, String> {
    let directory = light_dir(root);
    let target = directory.join(format!("{id}.mp4"));
    if super::valid_file(&target) {
        return Ok(target);
    }
    if let Some(why) = super::guard::blocked(root) {
        return Err(format!("no light copy: {why}"));
    }
    std::fs::create_dir_all(&directory).map_err(|error| format!("create light folder: {error}"))?;
    super::owned::mark(root, &format!("light/{id}.mp4"));
    let temporary = super::temp_sibling(&target);
    let ffmpeg = super::binary("RECALL_FFMPEG", "ffmpeg.exe")?;
    let lowered = super::drive_kind(read_from).is_hdd();
    let source_frames = super::ffmpeg::probe_frames(read_from, lowered);
    let limit = Duration::from_secs((seconds.max(10.0) as u64).saturating_mul(LIMIT_FACTOR) + 60);
    let mut result = Err("light copy failed".to_owned());
    for gpu in [true, false] {
        let mut command = Command::new(&ffmpeg);
        command.args(arguments(read_from, &temporary, gpu));
        result = match super::ffmpeg::run_pinned(&mut command, limit, stop) {
            Ok(output) if output.status.success() && super::valid_file(&temporary) => {
                if complete(source_frames, super::ffmpeg::probe_frames(&temporary, false)) {
                    super::ffmpeg::publish(&temporary, &target).map(|()| target.clone())
                } else {
                    Err("light copy came out short".to_owned())
                }
            }
            Ok(output) => Err(String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or("light copy failed").to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => Err(STOPPED.to_owned()),
            Err(error) => Err(format!("light copy: {error}")),
        };
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        // A stop or a time-out ends the item; only a failed or short GPU run tries again.
        if result.is_ok() || !gpu || result.as_ref().is_err_and(|e| e == STOPPED || e.contains("took too long")) {
            break;
        }
    }
    result
}

/// Clip length for the time limit, from the container (cheap) or the size.
pub(super) fn seconds_of(source: &Path, bytes: u64) -> f64 {
    super::probe_duration(source, false).unwrap_or(bytes as f64 / 3_300_000.0)
}

#[cfg(test)]
#[path = "light_tests.rs"]
mod tests;
