//! Editor preparation bridge and source-adjacent, validated project sidecars.
use serde_json::Value;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tauri::{AppHandle, Manager};

#[tauri::command]
pub async fn prepare_editor(app: AppHandle, path: String) -> Result<Value, String> {
    let source = canonical_file(&path)?;
    let cache = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("locate editor cache: {e}"))?
        .join("editor-media");
    let app_for_scope = app.clone();
    let source_for_worker = source.clone();
    let value = tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&cache).map_err(|e| format!("create editor cache: {e}"))?;
        let engine = locate_engine()?;
        let output = Command::new(engine)
            .args([
                "--prepare-editor",
                source_for_worker.to_string_lossy().as_ref(),
                cache.to_string_lossy().as_ref(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(0x08000000)
            .output()
            .map_err(|e| format!("start recorder preparation: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "recorder preparation failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|e| format!("recorder response was not UTF-8: {e}"))?;
        serde_json::from_str::<Value>(&text)
            .map_err(|e| format!("recorder response was not JSON: {e}"))
    })
    .await
    .map_err(|e| format!("editor preparation task failed: {e}"))??;
    allow_editor_assets(&app_for_scope, &source, &value)?;
    Ok(value)
}

#[tauri::command]
pub fn save_editor_project(path: String, project: Value) -> Result<String, String> {
    let source = canonical_file(&path)?;
    validate_project(&source, &project)?;
    let sidecar = sidecar_path(&source)?;
    let temp = sidecar.with_extension(format!("recall-edit.json.{}.tmp", std::process::id()));
    std::fs::write(
        &temp,
        serde_json::to_vec_pretty(&project).map_err(|e| format!("encode project: {e}"))?,
    )
    .map_err(|e| format!("write project sidecar: {e}"))?;
    std::fs::rename(&temp, &sidecar).map_err(|e| format!("publish project sidecar: {e}"))?;
    Ok(sidecar.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn load_editor_project(path: String) -> Result<Option<Value>, String> {
    let source = canonical_file(&path)?;
    let sidecar = sidecar_path(&source)?;
    if !sidecar.exists() {
        return Ok(None);
    }
    let project: Value = serde_json::from_slice(
        &std::fs::read(&sidecar).map_err(|e| format!("read project sidecar: {e}"))?,
    )
    .map_err(|e| format!("parse project sidecar: {e}"))?;
    validate_project(&source, &project)?;
    Ok(Some(project))
}

fn canonical_file(path: &str) -> Result<PathBuf, String> {
    let p = std::fs::canonicalize(path).map_err(|e| format!("resolve source {path}: {e}"))?;
    if !p.is_file() {
        return Err("editor source is not a file".into());
    }
    Ok(p)
}
fn sidecar_path(source: &Path) -> Result<PathBuf, String> {
    let name = source
        .file_name()
        .ok_or("source has no file name")?
        .to_string_lossy();
    Ok(source.with_file_name(format!("{name}.recall-edit.json")))
}
fn source_identity(source: &Path) -> Result<(u64, u64), String> {
    let meta = source.metadata().map_err(|e| format!("stat source: {e}"))?;
    let ms = meta
        .modified()
        .unwrap_or(std::time::UNIX_EPOCH)
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64;
    Ok((meta.len(), ms))
}

fn validate_project(source: &Path, project: &Value) -> Result<(), String> {
    let o = project
        .as_object()
        .ok_or("editor project must be an object")?;
    if o.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("editor project version is unsupported".into());
    }
    let (bytes, modified) = source_identity(source)?;
    if o.get("source_path").and_then(Value::as_str) != Some(source.to_string_lossy().as_ref())
        || o.get("source_bytes").and_then(Value::as_u64) != Some(bytes)
        || o.get("source_modified_ms").and_then(Value::as_u64) != Some(modified)
    {
        return Err("editor project belongs to a different source revision".into());
    }
    let plan = o
        .get("plan")
        .and_then(Value::as_object)
        .ok_or("editor project plan is invalid")?;
    let ranges = plan
        .get("ranges")
        .and_then(Value::as_array)
        .ok_or("editor project ranges are invalid")?;
    if ranges.is_empty() || ranges.len() > 1000 {
        return Err("editor project needs between 1 and 1000 ranges".into());
    }
    let mut previous_end = 0.0;
    for range in ranges {
        let range = range.as_object().ok_or("editor project range is invalid")?;
        let start = range
            .get("start")
            .and_then(Value::as_f64)
            .ok_or("editor project range start is invalid")?;
        let end = range
            .get("end")
            .and_then(Value::as_f64)
            .ok_or("editor project range end is invalid")?;
        if !start.is_finite()
            || !end.is_finite()
            || start < 0.0
            || end <= start
            || start < previous_end
        {
            return Err("editor project ranges must be ordered, finite, and positive".into());
        }
        previous_end = end;
    }
    let audio = plan
        .get("audio")
        .and_then(Value::as_array)
        .ok_or("editor project audio is invalid")?;
    let mut streams = std::collections::HashSet::new();
    for track in audio {
        let track = track
            .as_object()
            .ok_or("editor project audio track is invalid")?;
        let stream = track
            .get("stream_index")
            .and_then(Value::as_u64)
            .ok_or("editor project audio stream is invalid")?;
        let gain = track
            .get("gain_db")
            .and_then(Value::as_f64)
            .ok_or("editor project audio gain is invalid")?;
        if !gain.is_finite()
            || !(-60.0..=12.0).contains(&gain)
            || !track.get("muted").is_some_and(Value::is_boolean)
            || !streams.insert(stream)
        {
            return Err("editor project audio settings are invalid".into());
        }
    }
    if !o
        .get("export_fps")
        .and_then(Value::as_u64)
        .is_some_and(|fps| fps > 0 && fps <= i32::MAX as u64)
    {
        return Err("editor project export fps is invalid".into());
    }
    Ok(())
}

fn allow_editor_assets(app: &AppHandle, source: &Path, clip: &Value) -> Result<(), String> {
    let scope = app.asset_protocol_scope();
    scope
        .allow_file(source)
        .map_err(|e| format!("allow source preview: {e}"))?;
    let tracks = clip
        .get("audio")
        .and_then(Value::as_array)
        .ok_or("prepared clip is missing audio list")?;
    for track in tracks {
        let stem = track
            .get("preview_path")
            .and_then(Value::as_str)
            .ok_or("prepared audio has no preview path")?;
        scope
            .allow_file(stem)
            .map_err(|e| format!("allow generated audio preview: {e}"))?;
    }
    Ok(())
}

pub(crate) fn locate_engine() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("RECALL_ENGINE") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Ok(p);
        }
        return Err("RECALL_ENGINE does not name a file".into());
    }
    let exe = std::env::current_exe().map_err(|e| format!("locate Hub executable: {e}"))?;
    for ancestor in exe.ancestors() {
        for candidate in [
            ancestor.join("target-dev").join("debug").join("recall.exe"),
            ancestor.join("target").join("debug").join("recall.exe"),
            ancestor.join("recall.exe"),
        ] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err("could not locate recorder; set RECALL_ENGINE to its executable".into())
}
