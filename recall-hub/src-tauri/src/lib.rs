//! Recall Hub backend: a thin client of the recall-hdr daemon's control pipe.
//! Every pipe line is forwarded to the frontend as a `daemon` event; the
//! frontend sends commands back via the `daemon_send` invoke. The daemon owns
//! all real state — the hub renders it.

mod meters;
mod midi;

use std::io::{BufRead, BufReader, Write};
use std::sync::Mutex;

use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericNamespaced, SendHalf, Stream, ToNsName};
use tauri::{AppHandle, Emitter, Manager};

const PIPE_NAME: &str = "recall-hdr-control";

/// Writer half of the live pipe connection (None while disconnected).
struct PipeState(Mutex<Option<SendHalf>>);

/// Send one JSON command line to the daemon. Errors surface to the frontend.
#[tauri::command]
fn daemon_send(state: tauri::State<PipeState>, line: String) -> Result<(), String> {
    let mut guard = state.0.lock().map_err(|_| "pipe state poisoned".to_string())?;
    let Some(writer) = guard.as_mut() else {
        return Err("daemon not connected".into());
    };
    writer
        .write_all(line.as_bytes())
        .and_then(|_| writer.write_all(b"\n"))
        .map_err(|e| format!("pipe write failed: {e}"))
}

/// Newest clips in the recordings dir (name, mtime seconds, size, thumb path).
#[tauri::command]
fn list_clips(limit: usize) -> Vec<serde_json::Value> {
    let dir = std::env::var("RECALL_OUT_DIR").unwrap_or_else(|_| r"V:\ALL RECORDINGS".into());
    let mut clips: Vec<(std::time::SystemTime, serde_json::Value)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("mp4") {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        // Thumb convention: thumbs\<stem>-<epoch>-thumb.jpg (epoch unknown -> glob by prefix).
        let thumb = std::fs::read_dir(std::path::Path::new(&dir).join("thumbs"))
            .ok()
            .and_then(|it| {
                it.flatten()
                    .map(|t| t.path())
                    .find(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with(&stem))
                    })
            })
            .map(|p| p.to_string_lossy().into_owned());
        clips.push((
            modified,
            serde_json::json!({
                "name": path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
                "path": path.to_string_lossy(),
                "mtime": modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                "bytes": meta.len(),
                "thumb": thumb,
            }),
        ));
    }
    clips.sort_by(|a, b| b.0.cmp(&a.0));
    clips.into_iter().take(limit).map(|(_, v)| v).collect()
}

/// Reveal a clip in Explorer.
#[tauri::command]
fn show_in_folder(path: String) {
    let _ = std::process::Command::new("explorer")
        .args(["/select,", &path])
        .spawn();
}

/// Pipe client with auto-reconnect: forwards every line as a `daemon` event and
/// stores the writer half for `daemon_send`.
fn pipe_client_loop(app: AppHandle) {
    loop {
        let name = match PIPE_NAME.to_ns_name::<GenericNamespaced>() {
            Ok(n) => n,
            Err(_) => return,
        };
        match Stream::connect(name) {
            Ok(stream) => {
                let (reader, writer) = stream.split();
                *app.state::<PipeState>().0.lock().expect("pipe state") = Some(writer);
                let _ = app.emit("daemon", r#"{"mod":"hub","event":{"type":"connected"}}"#);
                // Ask for an immediate HDR snapshot for late joiners.
                {
                    let state = app.state::<PipeState>();
                    let mut g = state.0.lock().expect("pipe state");
                    if let Some(w) = g.as_mut() {
                        let _ = w.write_all(b"{\"cmd\":\"status\"}\n");
                    }
                }
                let reader = BufReader::new(reader);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if !line.trim().is_empty() {
                        let _ = app.emit("daemon", line.trim().to_string());
                    }
                }
                *app.state::<PipeState>().0.lock().expect("pipe state") = None;
                let _ = app.emit("daemon", r#"{"mod":"hub","event":{"type":"disconnected"}}"#);
            }
            Err(_) => {
                let _ = app.emit("daemon", r#"{"mod":"hub","event":{"type":"disconnected"}}"#);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(PipeState(Mutex::new(None)))
        .manage(midi::MidiState(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            daemon_send,
            list_clips,
            show_in_folder,
            midi::midi_ports,
            midi::midi_send
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            std::thread::spawn(move || pipe_client_loop(handle));
            meters::spawn_meters(app.handle().clone());
            midi::spawn_midi_monitor(app.handle().clone(), "rodecaster");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
