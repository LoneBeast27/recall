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
        /// Device tracks start held until the controller's `sync` (the sync handshake).
        sync_hold: bool,
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
        /// The last audio line's sequence number applied (the controller re-sends what is not).
        audio_seq: u64,
        /// What each recorded source opened, in track order (`audio::sources::opened`).
        #[serde(skip_serializing_if = "Vec::is_empty")]
        audio: Vec<AudioOpened>,
        /// The save the saver is writing now (absent when none is in the saver). Bytes that
        /// keep growing tell the controller the save is alive: a restart would abort it.
        #[serde(skip_serializing_if = "Option::is_none")]
        save_writing: Option<SaveWriting>,
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
    /// A `save_clip` failed. `rejected`: it failed before it was queued for
    /// writing (the newest request, while older saves may still be writing).
    SaveFailed {
        reason: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        rejected: bool,
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
    /// Every device track is held silent and opens no stream: stdin was not drained in time,
    /// or a read failed (merge-gate F7). The controller tells the user.
    InputsHeld {
        reason: String,
    },
    /// The startup hold ended (`sync` read, every track listed): the device tracks record.
    InputsReleased,
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

/// `status.save_writing`: the save in progress and the saves queued behind it. `step`
/// counts the saver's work (an AAC frame encoded, a packet muxed) and only grows, from
/// the audio encode on, before the `.part` exists; `bytes` = the packet bytes muxed so
/// far (about the `.part` size). Both come from the saver's counters, no file stat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SaveWriting {
    pub bytes: u64,
    pub queued: u32,
    pub step: u64,
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
    /// Silence is stored whatever Alt+M says (an unsure sibling held, or the startup hold): the
    /// Audio page meter shows "Held" from this, not from its own guess (merge-gate P2).
    pub held: bool,
    /// Alt+M silences it right now: the meter's "Muted" (design-audio-mute P20).
    pub mutes: bool,
    /// Retired (removed, disabled, re-picked): silent until the next engine (P11).
    pub retired: bool,
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
    /// Set microphone capture muting for newly captured replay audio. `press`: the user's Alt+M
    /// or toggle (the controller's `set_mic`); absent on a resync, which is never a press.
    SetMicMute {
        muted: bool,
        #[serde(default)]
        press: bool,
        /// The controller's sequence number for audio lines (`status.audio_seq` acks it).
        #[serde(default)]
        seq: Option<u64>,
    },
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
        /// The source is no longer recorded as this track (removed, disabled, re-picked under
        /// another name): it stores silence from its next packet and closes its stream for good.
        #[serde(default)]
        retire: Option<bool>,
        #[serde(default)]
        seq: Option<u64>,
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
    /// The controller restated its whole audio intent (mute, every source's flag and id):
    /// every line before this one was queued first, so once it is read the device tracks may
    /// record (`audio::sources::sync_read`). `seq`: the restatement's sequence number.
    Sync {
        #[serde(default)]
        seq: u64,
    },
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
/// (the controller is gone) is not a panic: the line is dropped and the engine is
/// marked orphaned (`stdin_reader::stdout_failed`); stdin EOF quits it after its saves.
pub fn emit(msg: &Outbound) {
    if let Some(observer) = OBSERVER.get() {
        observer(msg);
    }
    if let Ok(line) = serde_json::to_string(msg) {
        if !write_line(&mut std::io::stdout().lock(), &line) {
            crate::stdin_reader::stream_failed(crate::stdin_reader::Stream::Out);
        }
    }
}

/// One line to `out`; false when it could not be written (the controller is gone: the
/// line is dropped, never a panic as `println!` would).
pub fn write_line(out: &mut impl std::io::Write, line: &str) -> bool {
    writeln!(out, "{line}").and_then(|()| out.flush()).is_ok()
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
#[path = "protocol_tests.rs"]
mod tests;
