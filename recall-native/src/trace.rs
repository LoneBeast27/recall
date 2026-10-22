//! Test hook for the scripted viewer checks (`scripts/dev/viewer-check.ps1`): with
//! `RECALL_NATIVE_TRACE=<file>`, the editor appends one JSON line per poll (at most every
//! 30 ms) with the playback state, so a script can measure response times and stalls.
use std::{fs::File, io::Write, sync::Mutex, time::Instant};

use crate::editor_state::EditorState;

struct Trace {
    file: File,
    start: Instant,
}

static TRACE: Mutex<Option<Option<Trace>>> = Mutex::new(None);

pub fn record(ed: &EditorState) {
    let Ok(mut slot) = TRACE.lock() else { return };
    let trace = slot.get_or_insert_with(|| {
        let path = std::env::var_os("RECALL_NATIVE_TRACE")?;
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(Trace { file, start: Instant::now() })
    });
    let Some(trace) = trace.as_mut() else { return };
    let preview = ed.preview.as_ref();
    let line = serde_json::json!({
        "ms": trace.start.elapsed().as_millis() as u64,
        "clip": ed.info.as_ref().map(|i| i.path.as_str()),
        "ready": preview.is_some_and(|p| p.is_ready()),
        "playing": ed.playing,
        "native_playing": preview.is_some_and(|p| p.is_playing()),
        "audio": preview.map(|p| p.audio_position()),
        "video": preview.and_then(|p| p.video_position().ok()),
        "drift_seeks": preview.map(|p| p.drift_seeks.get()),
        "audio_stalls": preview.map(|p| p.audio_stalls.get()),
        "ranges": ed.plan.ranges.iter().map(|r| [r.start, r.end]).collect::<Vec<_>>(),
        "status": ed.status,
    });
    let _ = writeln!(trace.file, "{line}");
}

/// Always-on player log at `%APPDATA%\Recall\player.log` (stalls and recoveries), kept under
/// 512 KB, so a stall on the user's machine leaves evidence. Also mirrored into the test trace.
pub fn player_event(what: &str, detail: serde_json::Value) {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let line = serde_json::json!({ "unix_ms": stamp as u64, "event": what, "detail": detail });
    if let Some(root) = std::env::var_os("APPDATA") {
        let dir = std::path::PathBuf::from(root).join("Recall");
        let path = dir.join("player.log");
        let _ = std::fs::create_dir_all(&dir);
        if path.metadata().is_ok_and(|m| m.len() > 512 * 1024) {
            let _ = std::fs::rename(&path, dir.join("player.old.log"));
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(file, "{line}");
        }
    }
    if let Ok(mut slot) = TRACE.lock() {
        if let Some(Some(trace)) = slot.as_mut() {
            let _ = writeln!(trace.file, "{line}");
        }
    }
}
