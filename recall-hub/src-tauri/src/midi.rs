//! RODECaster MIDI surface: port enumeration, outbound messages (the RØDE
//! documented trigger path — Stream-Deck-style control), and an inbound
//! monitor (SMART pads sending MIDI to the PC) emitted as `midi-in` events.
//!
//! The exact trigger map is pinned WITH THE USER on the live unit (the RØDE
//! help doc enumerates it); until then the UI exposes raw note/CC send.

use std::sync::Mutex;

use midir::{MidiInput, MidiOutput, MidiOutputConnection};
use tauri::{AppHandle, Emitter};

pub struct MidiState(pub Mutex<Option<MidiOutputConnection>>);

#[tauri::command]
pub fn midi_ports() -> serde_json::Value {
    let ins = MidiInput::new("recall-hub-in")
        .map(|m| m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    let outs = MidiOutput::new("recall-hub-out")
        .map(|m| m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    serde_json::json!({ "inputs": ins, "outputs": outs })
}

/// Open (or reuse) the output whose name contains `port_match` and send bytes.
#[tauri::command]
pub fn midi_send(
    state: tauri::State<MidiState>,
    port_match: String,
    bytes: Vec<u8>,
) -> Result<(), String> {
    let mut guard = state.0.lock().map_err(|_| "midi state poisoned".to_string())?;
    if guard.is_none() {
        let out = MidiOutput::new("recall-hub-out").map_err(|e| e.to_string())?;
        let port = out
            .ports()
            .into_iter()
            .find(|p| {
                out.port_name(p)
                    .is_ok_and(|n| n.to_lowercase().contains(&port_match.to_lowercase()))
            })
            .ok_or_else(|| format!("no MIDI output matching '{port_match}'"))?;
        *guard = Some(out.connect(&port, "recall-hub").map_err(|e| e.to_string())?);
    }
    guard
        .as_mut()
        .expect("set above")
        .send(&bytes)
        .map_err(|e| e.to_string())
}

/// Listen on the first input matching `port_match`, emitting `midi-in` events.
pub fn spawn_midi_monitor(app: AppHandle, port_match: &'static str) {
    std::thread::Builder::new()
        .name("midi-in".into())
        .spawn(move || {
            let Ok(input) = MidiInput::new("recall-hub-in") else { return };
            // Prefer a name match; else take the first hardware input (the
            // RODECaster enumerates as e.g. "MIDI function").
            let ports = input.ports();
            let Some(port) = ports
                .iter()
                .find(|p| {
                    input
                        .port_name(p)
                        .is_ok_and(|n| n.to_lowercase().contains(port_match))
                })
                .or_else(|| ports.first())
                .cloned()
            else {
                return;
            };
            let name = input.port_name(&port).unwrap_or_default();
            let conn = input.connect(
                &port,
                "recall-hub-monitor",
                move |_ts, msg, app| {
                    let _ = app.emit(
                        "midi-in",
                        serde_json::json!({ "bytes": msg, "port": "rodecaster" }),
                    );
                },
                app.clone(),
            );
            if let Ok(_conn) = conn {
                let _ = app.emit(
                    "midi-in",
                    serde_json::json!({ "monitor": true, "port": name }),
                );
                // Park forever; the connection lives as long as this thread.
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
        })
        .ok();
}
