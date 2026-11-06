//! The long recording's notices. A recording whose stream changed goes on in
//! parts (`recording_stopped`/`recording_started` with `continued`): the
//! part's stop shows nothing, the next part's start shows ONE notice,
//! "Recording continues in part N", with what changed. The final stop shows the
//! whole length and how many files it took.
use serde_json::Value;

use crate::notice::Tone;
use crate::reducer::truncate;

/// A notice to show: title (also the primary line), its detail, the secondary line.
#[derive(Debug, Clone, PartialEq)]
pub struct RecNotice {
    pub tone: Tone,
    pub title: String,
    pub detail: String,
    pub text: String,
}

/// What changed, from the engine's `cause` ("size" | "input" | "encoder").
pub fn cause_text(cause: Option<&str>) -> &'static str {
    match cause {
        Some("size") => "The game's size changed",
        Some("input") => "Capture switched between GPU and CPU frames",
        Some("encoder") => "The video encoder restarted",
        _ => "The capture changed",
    }
}

/// `m:ss`, or `h:mm:ss` from an hour.
fn length(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// The notice for a `recording_started` / `recording_stopped` engine event (`kind`),
/// None for a continued part's stop. `name`: the event's shortened file name.
pub fn recording_notice(kind: &str, event: &Value, name: String) -> Option<RecNotice> {
    let continued = event.get("continued").and_then(Value::as_bool).unwrap_or(false);
    let num = |key: &str| event.get(key).and_then(Value::as_u64);
    match (kind, continued) {
        ("recording_stopped", true) => None,
        ("recording_started", true) => {
            let part = num("part").unwrap_or(2);
            let title = format!("Recording continues in part {part}");
            let detail = cause_text(event.get("cause").and_then(Value::as_str)).to_string();
            Some(RecNotice { tone: Tone::Recording, title, text: detail.clone(), detail })
        }
        ("recording_started", false) => {
            Some(RecNotice { tone: Tone::Recording, title: "Recording started".into(), text: format!("Writing {name}"), detail: name })
        }
        _ => {
            let parts = num("parts").unwrap_or(1).max(1);
            let seconds = event.get("total_seconds").or_else(|| event.get("seconds")).and_then(Value::as_f64).unwrap_or(0.0);
            let text = match parts {
                1 => truncate(&format!("{name} · {}", length(seconds)), 48),
                n => format!("{} · {n} files", length(seconds)),
            };
            Some(RecNotice { tone: Tone::Success, title: "Recording saved".into(), detail: text.clone(), text })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_single_file_recording_reads_as_before() {
        let ev = json!({"type":"recording_stopped","path":"E:/c/G__rec.mp4","seconds":75.0,"bytes":1,"continued":false});
        let n = recording_notice("recording_stopped", &ev, "G__rec.mp4".into()).unwrap();
        assert_eq!((n.title.as_str(), n.text.as_str()), ("Recording saved", "G__rec.mp4 · 1:15"));
        // An older engine sends neither `parts` nor `total_seconds`.
        let old = json!({"type":"recording_stopped","path":"E:/c/G__rec.mp4","seconds":3725.0,"bytes":1});
        assert_eq!(recording_notice("recording_stopped", &old, "G__rec.mp4".into()).unwrap().text, "G__rec.mp4 · 1:02:05");
    }
}
