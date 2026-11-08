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
        /// This engine applies `set_source` (gain, name) to live tracks. A controller restarts
        /// engines that do not say so (older builds ignore the command).
        live_sources: bool,
        /// This engine also applies `set_source`'s `mic_mute` and `device_id` live, and limits a
        /// gain boosted live (P2-a). An engine without it (8111e31 and older) ignores those
        /// fields, so the controller restarts it for them instead of sending them.
        live_source_fields: bool,
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
        /// "h264_nvenc" (replay ready, for every codec) | "degraded" (recording from CPU
        /// copies after GPU frames were refused; replay ready) | "recovering" (encoder
        /// being rebuilt; the buffer is kept and saveable) | "starting" | "off" | "failed"
        /// (the engine's own recovery is spent).
        encoder: String,
        encoder_error: Option<String>,
        /// "h264" | "hevc" | "av1": the codec recording (H.264 after a
        /// fallback), or the requested one before the encoder opens; "off"
        /// without an encoder.
        codec: String,
        recording_mode: String,
        /// Encoder-thread turns so far (about 10 a second, frames or not; 0 without an
        /// encoder). A beat that stops while the game is visible is a hung encoder.
        encoder_heartbeat: u64,
        /// An NVENC open (or reopen) is running now: a hang clock allows it longer.
        encoder_opening: bool,
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
        /// In-process only (bookmark placement): the clip's video end on the capture clock.
        #[serde(skip)]
        video_end_us: Option<u64>,
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
    /// An input opened on an unsure sibling was unmuted by a fresh Alt+M (A4 ruling): the
    /// controller saves `device_id` as source `id`'s own endpoint.
    SourceAdopted {
        id: String,
        device_id: String,
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
    /// `continued`: part `part` (2, 3, ...) of a running recording whose stream
    /// changed (`cause`: "size" | "input" (GPU / CPU frames) | "encoder"); it
    /// follows a `recording_stopped` with `continued`. A fresh start is part 1, cause null.
    RecordingStarted {
        path: String,
        continued: bool,
        part: u32,
        cause: Option<&'static str>,
    },
    /// `seconds`/`bytes`: this file. `parts`: files so far, `total_seconds`: all of them,
    /// `gap_seconds`: time not recorded between the parts (0 for one file); `frozen_seconds`:
    /// time the picture held still after a same-header encoder reopen (0 if none);
    /// `locked_seconds`: time the PC sat locked during the recording (`session_locked`; 0 if none).
    /// `continued`: only this part ended (`cause` as above); the recording goes on
    /// in a new file, reported by the next `recording_started`.
    RecordingStopped {
        path: String,
        seconds: f64,
        bytes: u64,
        continued: bool,
        parts: u32,
        total_seconds: f64,
        gap_seconds: f64,
        frozen_seconds: f64,
        locked_seconds: f64,
        cause: Option<&'static str>,
    },
    /// `path`: the partial recording that was kept (renamed to its final name),
    /// or null when nothing was written. `paths`: every file the recording left,
    /// the finished parts of a continued recording first (it ended early).
    /// `gap_seconds` / `frozen_seconds`: as `recording_stopped`, up to the failure.
    RecordingFailed {
        reason: String,
        path: Option<String>,
        paths: Vec<String>,
        gap_seconds: f64,
        frozen_seconds: f64,
    },
    /// Long recordings left as `.part` by a crash or a kill, renamed into place on this
    /// start: one event for all of them (`paths`; `path` is the first, for older readers).
    RecordingRecovered {
        path: String,
        paths: Vec<String>,
    },
    /// A `hud_probe` finished: where the game's HUD sits over the picture, one
    /// number per grid row (bit c = column c holds HUD). `ok` false: too little
    /// moving play to tell (`grid` empty, `reason` says why). `kind` "game" is
    /// steady HUD over a moving world; "ui" is a still picture (an app such as
    /// Discord, or a menu) whose dense edges and text count as occupied.
    /// `aspect` is the picture's width / height (0 when nothing was read).
    HudScan {
        ok: bool,
        kind: &'static str,
        cols: u32,
        rows: u32,
        grid: Vec<u64>,
        moving: u32,
        bursts: u32,
        aspect: f32,
        reason: Option<String>,
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
    /// An unsure sibling unmuted by a fresh Alt+M (A4 ruling): its endpoint id, for the
    /// controller to save as the source's own (`audio::hold`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adopt_id: Option<String>,
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
    SaveClip {
        seconds: Option<u32>,
        /// The game closed: end at its last frame (`SaveRequest::at_last_frame`).
        #[serde(default)]
        at_last_frame: bool,
    },
    /// Set microphone capture muting for newly captured replay audio.
    SetMicMute { muted: bool },
    /// Toggle microphone capture muting for newly captured replay audio.
    ToggleMicMute,
    /// The Audio page changed a recorded source's gain or name: applied to its live track,
    /// with no restart (the replay buffer stays). Unknown ids are ignored.
    SetSource {
        id: String,
        #[serde(default)]
        gain_db: Option<f32>,
        #[serde(default)]
        name: Option<String>,
        /// Whether the mic-mute hotkey silences this input (false: a line-in or capture card).
        #[serde(default)]
        mic_mute: Option<bool>,
        /// The endpoint id saved with the source's device (P1-B): the track reopens on it.
        #[serde(default)]
        device_id: Option<String>,
    },
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
    /// Find the game's HUD from a few frames (`hud_scan` answers; the island's Dynamic spot).
    HudProbe,
    /// Start / stop / toggle the manual long recording.
    RecordStart,
    RecordStop,
    RecordToggle,
    /// End a running long recording as failed now, keeping every part (the controller
    /// sends it before restarting a hung engine; the app says "Recording ended early").
    RecordFail { reason: String },
    /// The Windows session locked (Win+L) or unlocked: a long recording keeps going and
    /// counts the time (`recording_stopped.locked_seconds`).
    SessionLocked { locked: bool },
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

/// Serialize and print one outbound message as a JSON line. A closed stdout
/// (the controller is gone) is ignored, not a panic: stdin EOF quits cleanly.
pub fn emit(msg: &Outbound) {
    use std::io::Write;
    if let Some(observer) = OBSERVER.get() {
        observer(msg);
    }
    if let Ok(line) = serde_json::to_string(msg) {
        let _ = writeln!(std::io::stdout().lock(), "{line}");
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

    #[test]
    fn set_source_parses_with_either_field() {
        use super::Command;
        let both: Command = serde_json::from_str(r#"{"cmd":"set_source","id":"mic","gain_db":-3.5,"name":"Voice"}"#).unwrap();
        assert!(matches!(both, Command::SetSource { ref id, gain_db: Some(g), name: Some(ref n), mic_mute: None, device_id: None } if id == "mic" && g == -3.5 && n == "Voice"));
        let gain: Command = serde_json::from_str(r#"{"cmd":"set_source","id":"game","gain_db":6}"#).unwrap();
        assert!(matches!(gain, Command::SetSource { gain_db: Some(_), name: None, .. }));
        let line_in: Command = serde_json::from_str(r#"{"cmd":"set_source","id":"in-1","mic_mute":false}"#).unwrap();
        assert!(matches!(line_in, Command::SetSource { mic_mute: Some(false), gain_db: None, name: None, .. }));
    }

    #[test]
    fn record_fail_parses_with_its_reason() {
        // The controller ends a running long recording early before restarting a hung engine.
        let v = serde_json::from_str::<super::Command>(r#"{"cmd":"record_fail","reason":"the encoder isn't responding"}"#);
        assert!(matches!(v, Ok(super::Command::RecordFail { ref reason }) if reason == "the encoder isn't responding"), "not a record_fail");
    }

    #[test]
    fn session_locked_parses() {
        // The controller says when the Windows session locks / unlocks (audit U8).
        let v = serde_json::from_str::<super::Command>(r#"{"cmd":"session_locked","locked":true}"#);
        assert!(matches!(v, Ok(super::Command::SessionLocked { locked: true })), "not a session_locked");
    }

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
            video_end_us: Some(7),
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
            video_end_us: None,
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
