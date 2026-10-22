//! Engine protocol reader (vendor-parsed -- we do NOT import the `recall`
//! crate): relay every stdout line, cache hello/status, log the interesting
//! transitions, beep on save / capture outcomes, and drain stderr.

use std::io::{BufRead, BufReader};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::Shared;
use crate::log;

/// Compact digest of a status line; we only re-log when it changes.
#[derive(PartialEq, Eq, Clone, Default)]
struct StatusDigest {
    display: String,
    mode: String,
    source: String,
}

/// Read newline-delimited JSON from the engine's stdout: relay every line,
/// cache hello/status, log the interesting transitions.
pub(super) fn read_protocol(shared: &Arc<Shared>, stdout: std::process::ChildStdout) {
    let mut last: Option<StatusDigest> = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // ignore non-JSON / malformed lines
        };
        shared.hub_engine_event(line);
        shared.forward_event(&val);
        match val.get("type").and_then(|t| t.as_str()) {
            Some("hello") => {
                let version = val.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                let monitors = val.get("monitors").and_then(|m| m.as_array()).map(|a| a.len()).unwrap_or(0);
                log!("supervisor: engine hello -- version {version}, {monitors} monitor(s)");
                shared.cache.lock().unwrap_or_else(|p| p.into_inner()).hello = Some(line.to_string());
                shared.hello_count.fetch_add(1, Ordering::SeqCst);
            }
            Some("status") => {
                {
                    let mut cache = shared.cache.lock().unwrap_or_else(|p| p.into_inner());
                    cache.status = Some(line.to_string());
                    cache.view.encoder = val.get("encoder").and_then(|v| v.as_str()).map(str::to_owned);
                    if let Some(m) = val.get("mic_muted").and_then(|v| v.as_bool()) {
                        cache.view.mic_muted = Some(m);
                    }
                }
                let digest = StatusDigest {
                    display: field(&val, "display"),
                    mode: field(&val, "mode"),
                    source: field(&val, "source"),
                };
                // Log only on a real transition -- not every ~1/s fps tick.
                if last.as_ref() != Some(&digest) {
                    log!(
                        "supervisor: engine status -- display={} mode={} source={:?}",
                        digest.display,
                        digest.mode,
                        digest.source
                    );
                    last = Some(digest);
                }
            }
            Some("mic_muted") => {
                if let Some(m) = val.get("muted").and_then(|v| v.as_bool()) {
                    shared.cache.lock().unwrap_or_else(|p| p.into_inner()).view.mic_muted = Some(m);
                }
            }
            Some("saved") => {
                let path = field(&val, "path");
                let secs = val.get("seconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let ms = val.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                log!("supervisor: clip SAVED ({secs:.1}s, {ms}ms) -> {path}");
                crate::cues::saved();
            }
            Some("save_failed") => {
                log!("supervisor: clip save FAILED -- {}", field(&val, "reason"));
                beep_failure();
            }
            Some("encoder_log") => {
                log!("supervisor: engine encoder -- {}", field(&val, "message"));
            }
            Some(kind @ ("bookmarked" | "screenshot" | "recording_started" | "recording_stopped")) => {
                log!("supervisor: engine {kind} -- {}", field(&val, "path"));
                // A long recording ending is a finished file; the others were cued on the press.
                if kind == "recording_stopped" {
                    crate::cues::saved();
                }
            }
            Some("recording_recovered") => log!("supervisor: engine recovered a crashed long recording -> {}", field(&val, "path")),
            Some(kind @ ("screenshot_failed" | "recording_failed")) => {
                log!("supervisor: engine {kind} -- {}", field(&val, "reason"));
                beep_failure();
            }
            _ => {} // unknown message types are ignored
        }
    }
}

/// The failure sound, also used when no engine can save.
pub(crate) fn beep_failure() {
    crate::cues::failed();
}

/// Pull a string field from a JSON object, defaulting to empty.
fn field(val: &serde_json::Value, key: &str) -> String {
    val.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// Log the engine's stderr, line by line, so a broken engine is diagnosable.
pub(super) fn drain_stderr(stderr: std::process::ChildStderr) {
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if !line.is_empty() {
            log!("supervisor: engine-stderr: {line}");
        }
    }
}
