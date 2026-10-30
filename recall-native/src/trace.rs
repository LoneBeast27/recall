//! Test hook for the scripted viewer checks (`scripts/dev/viewer-check.ps1`): with
//! `RECALL_NATIVE_TRACE=<file>`, the app appends one JSON line per poll (at most every
//! 30 ms) with the route and playback state, so a script can measure response times and
//! stalls, plus where the clickable controls were painted so it can click them like a person.
use std::{
    fs::File,
    io::Write,
    sync::{Mutex, OnceLock},
    time::Instant,
};

use gpui::{IntoElement, Styled, canvas};

use crate::editor_state::EditorState;

struct Trace {
    file: File,
    start: Instant,
}

static TRACE: Mutex<Option<Option<Trace>>> = Mutex::new(None);
/// Click targets painted in the last frame: name → device-px [x, y, w, h].
static TARGETS: Mutex<Vec<(String, [i32; 4])>> = Mutex::new(Vec::new());

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RECALL_NATIVE_TRACE").is_some())
}

/// An invisible child that records where its parent was painted (device px) under `name`,
/// so a scripted check can click the same control a person does. Records nothing untraced.
pub fn target(name: impl Into<String>) -> impl IntoElement {
    let name = name.into();
    canvas(
        move |bounds, window, _| {
            if !enabled() {
                return;
            }
            let s = window.scale_factor();
            let px = |v: gpui::Pixels| (f32::from(v) * s).round() as i32;
            let rect = [px(bounds.origin.x), px(bounds.origin.y), px(bounds.size.width), px(bounds.size.height)];
            if let Ok(mut targets) = TARGETS.lock() {
                targets.retain(|(n, _)| *n != name);
                targets.push((name.clone(), rect));
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// Called at the start of every render: targets are re-recorded by the frame being built.
pub fn clear_targets() {
    if enabled() {
        if let Ok(mut targets) = TARGETS.lock() {
            targets.clear();
        }
    }
}

pub fn record(route: &str, ed: &EditorState) {
    let Ok(mut slot) = TRACE.lock() else { return };
    let trace = slot.get_or_insert_with(|| {
        let path = std::env::var_os("RECALL_NATIVE_TRACE")?;
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(Trace { file, start: Instant::now() })
    });
    let Some(trace) = trace.as_mut() else { return };
    let preview = ed.preview.as_ref();
    let pos = ed.position();
    let targets: serde_json::Map<String, serde_json::Value> =
        TARGETS.lock().map(|t| t.iter().map(|(n, r)| (n.clone(), serde_json::json!(r))).collect()).unwrap_or_default();
    let line = serde_json::json!({
        "ms": trace.start.elapsed().as_millis() as u64,
        "route": route,
        "clip": ed.info.as_ref().map(|i| i.path.as_str()),
        "duration": ed.info.as_ref().map(|i| i.duration),
        "ready": preview.is_some_and(|p| p.is_ready()),
        "playing": ed.playing,
        "native_playing": preview.is_some_and(|p| p.is_playing()),
        "video_state": preview.and_then(|p| p.video_state()),
        "pos": pos,
        "edited": ed.edited_time(pos),
        "kept": ed.plan.duration(),
        "audio": preview.map(|p| p.audio_position()),
        "video": preview.and_then(|p| p.video_position().ok()),
        "drift_seeks": preview.map(|p| p.drift_seeks.get()),
        "audio_stalls": preview.map(|p| p.audio_stalls.get()),
        "audio_underruns": preview.map(|p| p.audio_underruns()),
        "ranges": ed.plan.ranges.iter().map(|r| [r.start, r.end, r.speed]).collect::<Vec<_>>(),
        "selected": ed.selected,
        "undo": ed.can_undo(),
        "redo": ed.can_redo(),
        "stems": ed.info.as_ref().map(|i| i.audio.iter().map(|a| a.preview_path.as_str()).collect::<Vec<_>>()),
        "span": ed.track_span.get().map(|(left, width)| [left, width]),
        "status": ed.status,
        "targets": targets,
    });
    let _ = writeln!(trace.file, "{line}");
}

/// Logs preview-audio underruns (gaps heard) to the player log when the running total grows,
/// at most once every 5 s, so a "choppy sound" report leaves evidence.
pub fn note_underruns(clip: Option<&str>, total: u64) {
    static SEEN: Mutex<(u64, Option<Instant>)> = Mutex::new((0, None));
    let Ok(mut seen) = SEEN.lock() else { return };
    if total <= seen.0 || seen.1.is_some_and(|at| at.elapsed().as_secs() < 5) {
        return;
    }
    let new = total - seen.0;
    *seen = (total, Some(Instant::now()));
    drop(seen);
    player_event("audio underrun: the sound ran dry", serde_json::json!({ "clip": clip, "new": new, "total": total }));
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
