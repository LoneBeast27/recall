//! Read-only, bounded visual cache for the native editor.
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::backend::{cache_dir, VisualAssets};

const FRAME_COUNT: usize = 8;

pub(crate) fn thumbnail(path: &str) -> Result<String, String> {
    let source = source(path)?;
    if let Some(cached)=cached_thumbnail_for(&source,source.metadata().map_err(|error|format!("stat visual source: {error}"))?){return Ok(cached);}
    let duration = duration(&source).unwrap_or(0.0);
    frame(&source, "library", if duration > 0.0 { duration * 0.05 } else { 0.0 })
}
/// Metadata-only library lookup. This never launches ffprobe or ffmpeg.
pub(crate) fn cached_thumbnail(path:&Path,bytes:u64,modified_ms:u64)->Option<PathBuf>{
    let source=cache_source(path)?;
    let candidate=cache_dir().ok()?.join("visuals").join(identity_from(&source,bytes,modified_ms)).join("library.jpg");valid(&candidate).then_some(candidate)
}
fn cache_source(path:&Path)->Option<PathBuf>{std::fs::canonicalize(path).ok().filter(|source|source.is_file())}
fn cached_thumbnail_for(source:&Path,metadata:std::fs::Metadata)->Option<String>{
    let modified=metadata.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128)as u64;
    cached_thumbnail(source,metadata.len(),modified).map(|path|path.to_string_lossy().into_owned())
}

/// The successful frame paths are useful even if a later representative seek fails.
pub(crate) fn visuals_best_effort(path: &str, source_duration: f64) -> (VisualAssets, Option<String>) {
    let source = match source(path) { Ok(source) => source, Err(error) => return (VisualAssets::default(), Some(error)) };
    let duration = if source_duration.is_finite() && source_duration > 0.0 { source_duration } else {
        match duration(&source) { Ok(value) => value, Err(error) => return (VisualAssets::default(), Some(error)) }
    };
    let mut warnings = Vec::new();
    let poster = match frame(&source, "poster", duration * 0.05) { Ok(value) => Some(value), Err(error) => { warnings.push(format!("poster: {error}")); None } };
    let mut filmstrip = Vec::with_capacity(FRAME_COUNT);
    for index in 0..FRAME_COUNT {
        let seconds = duration * ((index as f64 + 0.5) / FRAME_COUNT as f64);
        match frame(&source, &format!("frame-{index}"), seconds) { Ok(value) => filmstrip.push(value), Err(error) => warnings.push(format!("frame {}: {error}", index + 1)) }
    }
    let warning = if warnings.is_empty() { None } else { Some(format!("Visual previews: {}", warnings.join("; "))) };
    (VisualAssets { poster, filmstrip }, warning)
}

fn source(path: &str) -> Result<PathBuf, String> {
    let source = std::fs::canonicalize(path).map_err(|error| format!("resolve visual source: {error}"))?;
    if !source.is_file() { return Err("visual source is not a file".into()); }
    Ok(source)
}

fn duration(source: &Path) -> Result<f64, String> {
    let output = hidden(ffprobe()?)
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
        .arg(source)
        .output()
        .map_err(|error| format!("start ffprobe: {error}"))?;
    if !output.status.success() { return Err(format!("ffprobe duration: {}", String::from_utf8_lossy(&output.stderr).trim())); }
    output.stdout.iter().map(|byte| *byte as char).collect::<String>().trim().parse::<f64>()
        .ok().filter(|value| value.is_finite() && *value > 0.0).ok_or_else(|| "ffprobe returned no usable duration".into())
}

fn frame(source: &Path, label: &str, seconds: f64) -> Result<String, String> {
    let key = identity(source)?;
    let directory = cache_dir()?.join("visuals").join(&key);
    std::fs::create_dir_all(&directory).map_err(|error| format!("create visual cache: {error}"))?;
    let target = directory.join(format!("{label}.jpg"));
    if valid(&target) { return Ok(target.to_string_lossy().into_owned()); }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let temporary = directory.join(format!("{label}-{}-{nonce}.jpg", std::process::id()));
    let seek = format!("{:.3}", seconds.max(0.0));
    let output = hidden(ffmpeg()?)
        .args(["-hide_banner", "-loglevel", "error", "-threads", "2", "-filter_threads", "1", "-ss", &seek, "-i"])
        .arg(source)
        .args(["-frames:v", "1", "-vf", "scale=320:-2", "-q:v", "4", "-y"])
        .arg(&temporary)
        .output()
        .map_err(|error| format!("start ffmpeg: {error}"))?;
    if !output.status.success() || !valid(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("ffmpeg frame: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    match std::fs::rename(&temporary, &target) {
        Ok(()) => Ok(target.to_string_lossy().into_owned()),
        Err(_) if valid(&target) => { let _ = std::fs::remove_file(&temporary); Ok(target.to_string_lossy().into_owned()) },
        Err(error) => { let _ = std::fs::remove_file(&temporary); Err(format!("publish visual cache: {error}")) },
    }
}

fn valid(path: &Path) -> bool { path.metadata().map(|metadata| metadata.is_file() && metadata.len() > 100).unwrap_or(false) }

fn identity(source: &Path) -> Result<String, String> {
    let metadata = source.metadata().map_err(|error| format!("stat visual source: {error}"))?;
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH).duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128)as u64;
    Ok(identity_from(source,metadata.len(),modified))
}
fn identity_from(source:&Path,bytes:u64,modified_ms:u64)->String{
    let mut hash = 0xcbf29ce484222325_u64;
    let display = source.to_string_lossy();
    hash_bytes(&mut hash, display.as_bytes());
    hash_bytes(&mut hash, &bytes.to_le_bytes());
    hash_bytes(&mut hash, &modified_ms.to_le_bytes());
    format!("{hash:016x}")
}
fn hash_bytes(hash:&mut u64,bytes:&[u8]){for byte in bytes{*hash^=*byte as u64;*hash=(*hash).wrapping_mul(0x100000001b3);}}

fn hidden(executable: PathBuf) -> Command { let mut command = Command::new(executable); command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(0x08004000); command }

fn ffmpeg() -> Result<PathBuf, String> { binary("RECALL_FFMPEG", "ffmpeg.exe") }
fn ffprobe() -> Result<PathBuf, String> { binary("RECALL_FFPROBE", "ffprobe.exe") }

fn binary(variable: &str, executable: &str) -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os(variable) { let path = PathBuf::from(value); return path.is_file().then_some(path).ok_or_else(|| format!("{variable} does not name {executable}")); }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let running_executable = std::env::current_exe().ok();
    let mut roots: Vec<&Path> = manifest.ancestors().collect();
    if let Some(path) = running_executable.as_deref() { roots.extend(path.ancestors()); }
    for root in roots {
        let dev = root.join("tools").join("ffmpeg-dev");
        if let Ok(entries) = std::fs::read_dir(&dev) {
            for entry in entries.flatten() { let candidate = entry.path().join("bin").join(executable); if candidate.is_file() { return Ok(candidate); } }
        }
        let candidate = root.join("bin").join(executable); if candidate.is_file() { return Ok(candidate); }
    }
    Err(format!("{executable} not found; set {variable} to the packaged binary"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn identity_changes_when_source_revision_changes() {
        let root = std::env::temp_dir().join(format!("recall-visual-test-{}", std::process::id())); std::fs::create_dir_all(&root).unwrap();
        let file = root.join("clip.mp4"); std::fs::write(&file, b"one").unwrap(); let first = identity(&file).unwrap();
        std::fs::write(&file, b"two more").unwrap(); assert_ne!(first, identity(&file).unwrap()); let _ = std::fs::remove_dir_all(root);
    }
    #[test] fn cached_lookup_canonicalizes_its_source_path() {
        let root=std::env::temp_dir().join(format!("recall-visual-path-test-{}",std::process::id()));std::fs::create_dir_all(&root).unwrap();let file=root.join("clip.mp4");std::fs::write(&file,b"source").unwrap();
        let noncanonical=root.join(".").join("clip.mp4");assert_eq!(cache_source(&noncanonical),Some(std::fs::canonicalize(&file).unwrap()));let _=std::fs::remove_dir_all(root);
    }
}
