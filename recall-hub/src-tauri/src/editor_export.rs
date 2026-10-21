//! Headless edited exports: the editor does not need an active capture session.
use serde_json::Value;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Manager};

static EXPORTING: AtomicBool = AtomicBool::new(false);
struct ExportGuard;
impl Drop for ExportGuard { fn drop(&mut self) { EXPORTING.store(false, Ordering::Release); } }

#[tauri::command]
pub async fn export_editor(app: AppHandle, path: String, fps: u32, plan: Value) -> Result<Value, String> {
    if fps == 0 || fps > i32::MAX as u32 { return Err("export fps must be a positive supported integer".into()); }
    if EXPORTING.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return Err("an editor export is already running".into());
    }
    let guard = ExportGuard;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let source = std::fs::canonicalize(&path).map_err(|e| format!("resolve source: {e}"))?;
        if !source.is_file() { return Err("editor source is not a file".into()); }
        let engine = crate::editor_media::locate_engine()?;
        let cache = app.path().app_cache_dir().map_err(|e| e.to_string())?.join("editor-jobs");
        std::fs::create_dir_all(&cache).map_err(|e| format!("create export job directory: {e}"))?;
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
        let project = cache.join(format!("{}-{nonce}.json", std::process::id()));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&project).map_err(|e| format!("create export job: {e}"))?;
        let result = (|| {
            use std::io::Write;
            file.write_all(&serde_json::to_vec(&plan).map_err(|e| e.to_string())?).map_err(|e| format!("write export plan: {e}"))?;
            file.flush().map_err(|e| e.to_string())?;
            let directory = source.parent().ok_or("source has no directory")?.join("exports");
            std::fs::create_dir_all(&directory).map_err(|e| format!("create exports directory: {e}"))?;
            let stem = source.file_stem().and_then(|v|v.to_str()).unwrap_or("clip");
            let destination = directory.join(format!("{stem}-edited-{fps}fps-{nonce}.mp4"));
            let output = Command::new(engine).arg("--export-edit").arg(&source).arg(&destination)
                .arg(fps.to_string()).arg(&project).arg("--json")
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
                .creation_flags(0x08000000).output().map_err(|e| format!("start editor export: {e}"))?;
            let events: Vec<Value> = String::from_utf8_lossy(&output.stdout).lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
            if !output.status.success() {
                let reason = events.iter().find(|v|v["type"] == "export_failed").and_then(|v|v["reason"].as_str());
                return Err(reason.map(str::to_owned).unwrap_or_else(||format!("editor export failed: {}", String::from_utf8_lossy(&output.stderr).trim())));
            }
            events.into_iter().find(|v|v["type"] == "exported").ok_or_else(||"recorder did not return an export result".into())
        })();
        drop(file);
        let _ = std::fs::remove_file(&project);
        result
    }).await.map_err(|e|format!("editor export task failed: {e}"))?
}
