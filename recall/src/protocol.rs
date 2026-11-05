//! Machine-readable JSON protocol spoken over stdout/stdin when `--json` is set.
//!
//! The Tauri control panel spawns the engine as a sidecar, parses these
//! newline-delimited JSON messages from stdout, and writes `Command`s to stdin.
//! Without `--json`, the engine prints human `[recall]` logs instead.

use serde::{Deserialize, Serialize};

/// Messages the engine emits on stdout (one JSON object per line).
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outbound {
    /// Sent once at startup: version + available monitors.
    Hello {
        version: String,
        monitors: Vec<MonitorInfo>,
    },
    /// Sent on change (and ~1/s for fps): current live state.
    Status {
        /// "hdr" | "sdr" — the detected display color space.
        display: String,
        max_nits: f32,
        /// Effective tonemap mode: "passthrough" | "tonemap".
        mode: String,
        /// Mode selector: "auto" | "sdr" | "hdr".
        select: String,
        /// Current `<source>` (without the " SDR" suffix).
        source: String,
        width: u32,
        height: u32,
        fps: u32,
        /// Seconds of replay currently buffered (0.0 when encode is off/down).
        buffer_secs: f32,
        /// "h264_nvenc" (replay ready, for every codec) | "starting" | "off" | "failed".
        encoder: String,
        encoder_error: Option<String>,
        /// "h264" | "hevc" | "av1": the codec recording (H.264 after a
        /// fallback), or the requested one before the encoder opens; "off"
        /// without an encoder.
        codec: String,
        recording_mode: String,
        /// Whether new microphone samples are entering the replay buffer as silence.
        mic_muted: bool,
        /// What each recorded source opened, in track order (`audio::sources::opened`).
        #[serde(skip_serializing_if = "Vec::is_empty")]
        audio: Vec<AudioOpened>,
    },
    /// A `save_clip` completed: the clip is on disk. `path` is always the
    /// clip's FINAL place in the clip folder. With `staged: true` the finished
    /// clip sits in `staged_path` (SSD staging) and a background move puts it
    /// at `path`; `clip_moved` / `clip_move_failed` follow. With `staged:
    /// false` (no `staged_path`) it was written to `path` directly.
    Saved {
        path: String,
        seconds: f32,
        frames: u64,
        bytes: u64,
        elapsed_ms: u64,
        staged: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        staged_path: Option<String>,
    },
    /// A staged clip is now at its final `path`. `elapsed_ms`: this move's
    /// copy + flush + rename time. After a copy across volumes the staged
    /// copy is kept at `kept_path` (`<staging>\kept\<final name>`, same
    /// bytes and modification time as `path`) for the app to adopt into its
    /// SSD video cache; without `kept_path` the staged copy is gone.
    ClipMoved {
        path: String,
        elapsed_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        kept_path: Option<String>,
    },
    /// A staged clip could not be moved to `path`; it stays complete in
    /// `staged_path` and the move is retried (every 60 s, on the next save,
    /// and at the next engine start).
    ClipMoveFailed {
        path: String,
        staged_path: String,
        reason: String,
    },
    /// A `save_clip` failed.
    SaveFailed {
        reason: String,
    },
    /// Sent after microphone capture muting changes.
    MicMuted {
        muted: bool,
    },
    ExportStarted {
        source: String,
        path: String,
        fps: u32,
    },
    Exported {
        source: String,
        path: String,
        fps: u32,
        frames: u64,
        seconds: f64,
        elapsed_ms: u64,
    },
    ExportFailed {
        source: String,
        reason: String,
    },
    /// Encoder lifecycle/diagnostic line (machine-readable log).
    EncoderLog {
        message: String,
    },
    /// A `bookmark` landed at this capture-clock time (seconds since capture
    /// start). Saved clips covering it get `<stem>.recall-marks.json`.
    Bookmarked {
        at_secs: f64,
    },
    /// A `screenshot` PNG is on disk.
    Screenshot {
        path: String,
    },
    ScreenshotFailed {
        reason: String,
    },
    /// A manual long recording is writing; `path` is the final name (the file
    /// is `<path>.part` until it stops).
    RecordingStarted {
        path: String,
    },
    RecordingStopped {
        path: String,
        seconds: f64,
        bytes: u64,
    },
    /// `path`: the partial recording that was kept (renamed to its final name),
    /// or null when nothing was written.
    RecordingFailed {
        reason: String,
        path: Option<String>,
    },
    /// A long recording left as `.part` by a crash was renamed into place.
    RecordingRecovered {
        path: String,
    },
}

/// One recorded source in `status.audio`: what it captures right now.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AudioOpened {
    /// The source's id in `recording.audio_sources`.
    pub id: String,
    pub name: String,
    /// The endpoint or process captured ("… (Windows default)" after a fallback), or why
    /// the source has no track.
    pub opened: String,
    pub live: bool,
}

#[derive(Serialize)]
pub struct MonitorInfo {
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

/// Commands the engine accepts on stdin (one JSON object per line).
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// value: "auto" | "sdr" | "hdr"
    SetMode { value: String },
    /// Save the last N seconds of the replay buffer (default: full buffer).
    SaveClip { seconds: Option<u32> },
    /// Set microphone capture muting for newly captured replay audio.
    SetMicMute { muted: bool },
    /// Toggle microphone capture muting for newly captured replay audio.
    ToggleMicMute,
    /// Export an existing VFR clip to a separate NVENC CFR file (60 fps default).
    ExportClip {
        path: String,
        output: Option<String>,
        fps: Option<u32>,
        #[serde(default)]
        edit: Option<crate::edit_model::EditPlan>,
    },
    /// Mark "now" for the clips saved later (bookmark hotkey).
    Bookmark,
    /// PNG of the next rendered capture frame into `<out_dir>\screenshots\`.
    Screenshot,
    /// Start / stop / toggle the manual long recording.
    RecordStart,
    RecordStop,
    RecordToggle,
    /// Ask the engine to exit cleanly.
    Quit,
}

type Observer = Box<dyn Fn(&Outbound) + Send + Sync>;
static OBSERVER: std::sync::OnceLock<Observer> = std::sync::OnceLock::new();

/// Install the one in-process listener that sees every message just before it
/// is printed (the bookmark sidecar is written off `Saved` this way, so the
/// file exists by the time a client reads the event). Later calls are ignored.
pub fn set_observer(observer: impl Fn(&Outbound) + Send + Sync + 'static) {
    let _ = OBSERVER.set(Box::new(observer));
}

/// Serialize and print one outbound message as a JSON line.
pub fn emit(msg: &Outbound) {
    if let Some(observer) = OBSERVER.get() {
        observer(msg);
    }
    if let Ok(line) = serde_json::to_string(msg) {
        println!("{line}");
    }
}

/// `emit` in sidecar (`--json`) mode, else a human `[recall]` line on stderr.
pub fn emit_or_log(json: bool, msg: &Outbound, human: impl FnOnce() -> String) {
    if json {
        emit(msg);
    } else {
        eprintln!("[recall] {}", human());
    }
}

#[cfg(test)]
mod tests {
    use super::Outbound;

    fn wire(msg: &Outbound) -> String {
        serde_json::to_string(msg).unwrap()
    }

    #[test]
    fn saved_keeps_its_fields_and_adds_staging() {
        let staged = Outbound::Saved {
            path: r"E:\Clips\G__x.mp4".into(),
            seconds: 120.0,
            frames: 7200,
            bytes: 400,
            elapsed_ms: 812,
            staged: true,
            staged_path: Some(r"F:\Recall\cache\staging\G__x.mp4".into()),
        };
        assert_eq!(
            wire(&staged),
            r#"{"type":"saved","path":"E:\\Clips\\G__x.mp4","seconds":120.0,"frames":7200,"bytes":400,"elapsed_ms":812,"staged":true,"staged_path":"F:\\Recall\\cache\\staging\\G__x.mp4"}"#
        );
        let direct = Outbound::Saved {
            path: "a.mp4".into(),
            seconds: 1.5,
            frames: 3,
            bytes: 10,
            elapsed_ms: 5,
            staged: false,
            staged_path: None,
        };
        assert_eq!(
            wire(&direct),
            r#"{"type":"saved","path":"a.mp4","seconds":1.5,"frames":3,"bytes":10,"elapsed_ms":5,"staged":false}"#
        );
    }

    #[test]
    fn clip_move_events_match_the_wire_shape() {
        let moved = Outbound::ClipMoved { path: r"E:\c\G.mp4".into(), elapsed_ms: 9549, kept_path: None };
        assert_eq!(wire(&moved), r#"{"type":"clip_moved","path":"E:\\c\\G.mp4","elapsed_ms":9549}"#);
        let kept = Outbound::ClipMoved { path: r"E:\c\G.mp4".into(), elapsed_ms: 9549, kept_path: Some(r"F:\s\kept\G.mp4".into()) };
        assert_eq!(wire(&kept), r#"{"type":"clip_moved","path":"E:\\c\\G.mp4","elapsed_ms":9549,"kept_path":"F:\\s\\kept\\G.mp4"}"#);
        let failed = Outbound::ClipMoveFailed {
            path: r"E:\c\G.mp4".into(),
            staged_path: r"F:\s\G.mp4".into(),
            reason: "disk full".into(),
        };
        assert_eq!(
            wire(&failed),
            r#"{"type":"clip_move_failed","path":"E:\\c\\G.mp4","staged_path":"F:\\s\\G.mp4","reason":"disk full"}"#
        );
    }
}
