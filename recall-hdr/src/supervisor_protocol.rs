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

/// Read newline-delimited JSON from the engine's stdout until it ends: relay every line,
/// cache hello/status, log the interesting transitions. A line that is not UTF-8 (a C
/// library printing to fd 1) is decoded lossily and skipped as malformed: the reader
/// never stops early (the engine's next write would fail and orphan it).
pub(super) fn read_protocol(shared: &Arc<Shared>, stdout: impl std::io::Read) {
    let mut last: Option<StatusDigest> = None;
    drain_lines(BufReader::new(stdout), |line| protocol_line(shared, line, &mut last));
}

/// One stdout line (trimmed, not empty).
fn protocol_line(shared: &Arc<Shared>, line: &str, last: &mut Option<StatusDigest>) {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
        return; // ignore non-JSON / malformed lines
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
            shared.on_hello_floor(&val); // P14: no sync_hold -> restarted with no device
        }
        Some("status") => {
            {
                let mut cache = shared.cache.lock().unwrap_or_else(|p| p.into_inner());
                cache.status = Some(line.to_string());
                cache.view.statuses += 1;
                cache.view.encoder = val.get("encoder").and_then(|v| v.as_str()).map(str::to_owned);
                if let Some(m) = val.get("mic_muted").and_then(|v| v.as_bool()) {
                    cache.view.mic_muted = Some(m);
                }
                cache.view.buffer_secs = val.get("buffer_secs").and_then(|v| v.as_f64()).map(|b| b as f32);
                cache.view.heartbeat = val.get("encoder_heartbeat").and_then(|v| v.as_u64());
                cache.view.opening = val.get("encoder_opening").and_then(|v| v.as_bool()).unwrap_or(false);
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
                *last = Some(digest);
            }
        }
        // A released engine's drain heartbeat (design P11): it keeps the engine (P10).
        Some("save_progress") => shared.note_beat(),
        Some("mic_muted") => {
            if let Some(m) = val.get("muted").and_then(|v| v.as_bool()) {
                shared.cache.lock().unwrap_or_else(|p| p.into_inner()).view.mic_muted = Some(m);
            }
        }
        Some("saved") => {
            let path = field(&val, "path");
            let secs = val.get("seconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let ms = val.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);
            let staged = match val.get("staged").and_then(|v| v.as_bool()) {
                Some(true) => format!(" (staged at {}, moving in the background)", field(&val, "staged_path")),
                _ => String::new(),
            };
            log!("supervisor: clip SAVED ({secs:.1}s, {ms}ms) -> {path}{staged}");
            crate::cues::saved();
        }
        Some("save_failed") => {
            log!("supervisor: clip save FAILED -- {}", field(&val, "reason"));
            beep_failure();
        }
        // A staged clip's background move (relayed to app clients like every line).
        Some("clip_moved") => {
            let ms = val.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);
            log!("supervisor: clip moved into the clip folder ({ms}ms) -> {}", field(&val, "path"));
        }
        Some("clip_move_failed") => {
            log!(
                "supervisor: clip move FAILED -- {} (kept at {}, retrying) -> {}",
                field(&val, "reason"),
                field(&val, "staged_path"),
                field(&val, "path")
            );
            beep_failure();
        }
        Some("encoder_log") => {
            let message = field(&val, "message");
            if !shared.slot.mutes(&message) {
                log!("supervisor: engine encoder -- {message}");
            }
        }
        Some(kind @ ("bookmarked" | "screenshot" | "recording_started" | "recording_stopped")) => {
            log!("supervisor: engine {kind} -- {}", field(&val, "path"));
            // A long recording ending is a finished file; the others were cued on the press.
            // A continued part (`continued`, the capture size changed) is not the end.
            let continued = val.get("continued").and_then(|v| v.as_bool()).unwrap_or(false);
            if kind == "recording_stopped" && !continued {
                crate::cues::saved();
            }
        }
        Some("hud_scan") => log!(
            "supervisor: engine hud scan -- ok={} kind={} moving={}/{} {}",
            val.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
            val.get("kind").and_then(|v| v.as_str()).unwrap_or("game"),
            val.get("moving").and_then(|v| v.as_u64()).unwrap_or(0),
            val.get("bursts").and_then(|v| v.as_u64()).unwrap_or(0),
            field(&val, "reason")
        ),
        Some("recording_recovered") => log!("supervisor: engine recovered a crashed long recording -> {}", field(&val, "path")),
        Some(kind @ ("screenshot_failed" | "recording_failed")) => {
            log!("supervisor: engine {kind} -- {}", field(&val, "reason"));
            beep_failure();
        }
        _ => {} // unknown message types are ignored
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
    drain_lines(BufReader::new(stderr), |line| log!("supervisor: engine-stderr: {line}"));
}

/// Every non-empty, trimmed line of `reader` to `sink`, until EOF or a read error. Read as
/// bytes: a line that is not UTF-8 (FFmpeg's C logger can print one) is shown lossily, never
/// taken as the end, or the engine would block on its next log once the pipe filled.
fn drain_lines(mut reader: impl BufRead, mut sink: impl FnMut(&str)) {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if !line.is_empty() {
            sink(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::drain_lines;

    #[test]
    fn a_line_that_is_not_utf8_does_not_stop_the_stderr_drain() {
        let bytes: &[u8] = b"first\n\xff\xfe ffmpeg \xe9\n\nthird\r\nlast without newline";
        let mut got = Vec::new();
        drain_lines(bytes, |line| got.push(line.to_string()));
        assert_eq!(got, ["first", "\u{fffd}\u{fffd} ffmpeg \u{fffd}", "third", "last without newline"]);
    }
}

#[cfg(test)]
mod drain_tests {
    use super::drain_lines;

    #[test]
    fn a_line_that_is_not_utf8_never_stops_the_stdout_reader() {
        // R7-overlay-6: a stopped stdout reader orphans the engine.
        let shared = std::sync::Arc::new(super::Shared::new(None));
        super::read_protocol(&shared, &b"\xff\xfe junk\n{\"type\":\"hello\",\"version\":\"t\"}\n"[..]);
        assert_eq!(shared.hello_count.load(std::sync::atomic::Ordering::SeqCst), 1, "the hello after the bad line is read");
    }

    #[test]
    fn a_line_that_is_not_utf8_never_stops_the_stderr_reader() {
        // R6-overlay-4: a stopped reader breaks the engine's stderr pipe.
        let mut seen = Vec::new();
        drain_lines(&b"\xff\xfe bad\nok\n\n  last  \n"[..], |l| seen.push(l.to_string()));
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(seen[0].ends_with("bad"), "lossy: {:?}", seen[0]);
        assert_eq!(&seen[1..], ["ok", "last"]);
    }
}
