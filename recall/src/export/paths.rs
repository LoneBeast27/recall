//! Source/destination protection and atomic publish shared by every export.

use std::path::{Path, PathBuf};

use ffmpeg_next as ff;

use super::ExportRequest;

/// Validate the request, run `work(input, temp)` against a reserved hidden temp
/// file beside the destination, then rename it into place. The temp file is
/// removed on any failure, and an existing destination is never overwritten.
pub(super) fn publish<T>(
    req: &ExportRequest,
    work: impl FnOnce(&Path, &Path) -> Result<T, String>,
) -> Result<T, String> {
    validate_request(req)?;
    ff::init().map_err(|e| format!("FFmpeg initialization failed: {e}"))?;

    let input = canonical_input(Path::new(&req.input))?;
    let output = absolute_output(Path::new(&req.output))?;
    if input == output {
        return Err("input and output must be different files".into());
    }
    if output.exists() {
        return Err(format!("destination already exists: {}", output.display()));
    }
    let temp = temporary_output_path(&output)?;

    match work(&input, &temp) {
        Ok(exported) => {
            if output.exists() {
                let _ = std::fs::remove_file(&temp);
                return Err(format!(
                    "destination appeared during export: {}",
                    output.display()
                ));
            }
            std::fs::rename(&temp, &output).map_err(|e| {
                let _ = std::fs::remove_file(&temp);
                format!("publish {}: {e}", output.display())
            })?;
            Ok(exported)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

fn validate_request(req: &ExportRequest) -> Result<(), String> {
    if req.fps == 0 {
        return Err("export fps must be a positive integer".into());
    }
    if req.fps > i32::MAX as u32 {
        return Err(format!(
            "export fps {} exceeds FFmpeg's supported integer range",
            req.fps
        ));
    }
    if req.input.trim().is_empty() {
        return Err("input path is empty".into());
    }
    if req.output.trim().is_empty() {
        return Err("output path is empty".into());
    }
    Ok(())
}

fn canonical_input(path: &Path) -> Result<PathBuf, String> {
    if !path.is_file() {
        return Err(format!("input video does not exist: {}", path.display()));
    }
    std::fs::canonicalize(path).map_err(|e| format!("resolve input {}: {e}", path.display()))
}

fn absolute_output(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| "output path has no file name".to_string())?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = std::fs::canonicalize(parent)
        .map_err(|e| format!("resolve output directory {}: {e}", parent.display()))?;
    if !parent.is_dir() {
        return Err(format!(
            "output directory is not a directory: {}",
            parent.display()
        ));
    }
    Ok(parent.join(name))
}

fn temporary_output_path(output: &Path) -> Result<PathBuf, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let parent = output
        .parent()
        .ok_or_else(|| "output path has no parent directory".to_string())?;
    let stem = output
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("export");
    let extension = output.extension().and_then(|s| s.to_str()).unwrap_or("mp4");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock before Unix epoch: {e}"))?
        .as_nanos();
    for attempt in 0..32u32 {
        let candidate = parent.join(format!(
            ".{stem}.recall-export-{}-{nonce}-{attempt}.{extension}",
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                drop(file);
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "reserve temporary output {}: {error}",
                    candidate.display()
                ))
            }
        }
    }
    Err("could not allocate a unique temporary export path".into())
}

/// Size of a finished temporary file.
pub(super) fn file_bytes(path: &Path) -> Result<u64, String> {
    std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| format!("stat export {}: {e}", path.display()))
}
