//! Test hook for the scripted viewer checks (`scripts/dev/viewer-check.ps1`): with
//! `RECALL_NATIVE_TRACE=<file>`, the app appends one JSON line per poll (at most every
//! 30 ms) with the route and playback state, so a script can measure response times and
//! stalls, plus where the clickable controls were painted so it can click them like a person.
//!
//! `RECALL_NATIVE_LAYOUT=<file>` keeps one JSON document of the current layout for an external
//! tester (GPUI has no accessibility tree). Dev format, no contract: `route`, `clips`, `scale`,
//! `window` (client-area origin, screen px) and `targets` (name -> client px [x, y, w, h]).
//! Replaced atomically when the layout changes, at most every 100 ms.
use std::{
    fs::File,
    io::Write,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicIsize, Ordering},
    },
    time::{Duration, Instant},
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
/// Window of the last render, and its scale x 1000 (layout dump only).
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static SCALE_MILLI: AtomicIsize = AtomicIsize::new(1000);

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RECALL_NATIVE_TRACE").is_some() || layout_path().is_some())
}

fn layout_path() -> Option<&'static PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| std::env::var_os("RECALL_NATIVE_LAYOUT").filter(|v| !v.is_empty()).map(PathBuf::from)).as_ref()
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
pub fn clear_targets(window: &gpui::Window) {
    if enabled() {
        if let Ok(mut targets) = TARGETS.lock() {
            targets.clear();
        }
        if layout_path().is_some() {
            WINDOW.store(crate::root::parent_hwnd(window).unwrap_or(0), Ordering::Relaxed);
            SCALE_MILLI.store((window.scale_factor() * 1000.0) as isize, Ordering::Relaxed);
        }
    }
}

/// Client-area origin of the window on screen.
fn client_origin() -> [i32; 2] {
    use windows::Win32::{Foundation::{HWND, POINT}, Graphics::Gdi::ClientToScreen};
    let mut origin = POINT::default();
    let _ = unsafe { ClientToScreen(HWND(WINDOW.load(Ordering::Relaxed) as *mut _), &mut origin) };
    [origin.x, origin.y]
}

/// Called every poll; returns at once unless `RECALL_NATIVE_LAYOUT` is set.
pub fn dump_layout(route: &str, clips: usize) {
    static LAST: Mutex<Option<(Instant, String)>> = Mutex::new(None);
    let Some(path) = layout_path() else { return };
    let Ok(mut last) = LAST.lock() else { return };
    if last.as_ref().is_some_and(|(at, _)| at.elapsed() < Duration::from_millis(100)) {
        return;
    }
    let targets: serde_json::Map<String, serde_json::Value> =
        TARGETS.lock().map(|t| t.iter().map(|(n, r)| (n.clone(), serde_json::json!(r))).collect()).unwrap_or_default();
    let scale = SCALE_MILLI.load(Ordering::Relaxed) as f64 / 1000.0;
    let text = serde_json::json!({ "route": route, "clips": clips, "scale": scale, "window": client_origin(), "targets": targets }).to_string();
    if last.as_ref().is_some_and(|(_, written)| *written == text) {
        return;
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &text).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        *last = Some((Instant::now(), text));
    }
}

/// One line in the test trace, if there is one.
pub fn note(what: &str) {
    let Ok(mut slot) = TRACE.lock() else { return };
    if let Some(Some(trace)) = slot.as_mut() {
        let _ = writeln!(trace.file, "{}", serde_json::json!({ "ms": trace.start.elapsed().as_millis() as u64, "note": what }));
    }
}

pub fn record(route: &str, ed: &EditorState, clips: usize) {
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
        "clips": clips,
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
        "picture_end": preview.and_then(|p| p.picture_end()),
        "drift_seeks": preview.map(|p| p.drift_seeks.get()),
        "audio_stalls": preview.map(|p| p.audio_stalls.get()),
        "audio_underruns": preview.map(|p| p.audio_underruns()),
        "ranges": ed.plan.ranges.iter().map(|r| [r.start, r.end, r.speed]).collect::<Vec<_>>(),
        "selected": ed.selected,
        "undo": ed.can_undo(),
        "redo": ed.can_redo(),
        "stems": ed.info.as_ref().map(|i| i.audio.iter().map(|a| a.preview_path.as_str()).collect::<Vec<_>>()),
        "source": ed.playback_path,
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
