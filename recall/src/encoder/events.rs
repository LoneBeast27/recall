//! Where save and move events go: sidecar JSON on stdout, human `[recall]`
//! lines on stderr, or (tests) a capture buffer holding exactly those lines.
//! Shared by `recall-saver` and the `recall-mover` thread.

use std::sync::{Arc, Mutex};

use crate::protocol::Outbound;

#[derive(Clone)]
pub(crate) struct Events {
    json: bool,
    sink: Option<Arc<Mutex<Vec<String>>>>,
}

/// What a finished save reports (`Outbound::Saved`).
pub(crate) struct SavedReport<'a> {
    /// The clip's FINAL path in the clip folder.
    pub path: &'a str,
    /// Where the finished clip sits until the background move, if staged.
    pub staged_path: Option<&'a str>,
    pub seconds: f32,
    pub frames: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
}

impl Events {
    pub fn new(json: bool) -> Self {
        Self { json, sink: None }
    }

    #[cfg(test)]
    pub fn capture(json: bool) -> (Self, Arc<Mutex<Vec<String>>>) {
        let sink = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                json,
                sink: Some(sink.clone()),
            },
            sink,
        )
    }

    fn out(&self, msg: Outbound, human: impl FnOnce() -> String) {
        match &self.sink {
            None if self.json => crate::protocol::emit(&msg),
            None => eprintln!("{}", human()),
            Some(sink) => {
                let line = if self.json {
                    serde_json::to_string(&msg).unwrap_or_default()
                } else {
                    human()
                };
                sink.lock().unwrap_or_else(|p| p.into_inner()).push(line);
            }
        }
    }

    pub fn saved(&self, r: &SavedReport) {
        let msg = Outbound::Saved {
            path: r.path.to_string(),
            seconds: r.seconds,
            frames: r.frames,
            bytes: r.bytes,
            elapsed_ms: r.elapsed_ms,
            staged: r.staged_path.is_some(),
            staged_path: r.staged_path.map(str::to_string),
        };
        self.out(msg, || {
            let (path, seconds, frames, bytes, ms) = (r.path, r.seconds, r.frames, r.bytes, r.elapsed_ms);
            let staged = r.staged_path.map(|s| format!(" [staged at {s}]")).unwrap_or_default();
            format!("[recall] saved {path} ({seconds:.1}s, {frames} frames, {bytes} bytes, {ms}ms){staged}")
        });
    }

    pub fn save_failed(&self, reason: &str) {
        let msg = Outbound::SaveFailed {
            reason: reason.to_string(),
        };
        self.out(msg, || format!("[recall] save FAILED: {reason}"));
    }

    pub fn clip_moved(&self, path: &str, elapsed_ms: u64, kept_path: Option<&str>) {
        let msg = Outbound::ClipMoved {
            path: path.to_string(),
            elapsed_ms,
            kept_path: kept_path.map(str::to_string),
        };
        self.out(msg, || format!("[recall] moved clip to {path} ({elapsed_ms}ms)"));
    }

    pub fn clip_move_failed(&self, path: &str, staged_path: &str, reason: &str) {
        let msg = Outbound::ClipMoveFailed {
            path: path.to_string(),
            staged_path: staged_path.to_string(),
            reason: reason.to_string(),
        };
        self.out(msg, || {
            format!("[recall] clip move FAILED ({reason}): {staged_path} stays staged for {path}")
        });
    }

    pub fn log(&self, message: &str) {
        let msg = Outbound::EncoderLog {
            message: message.to_string(),
        };
        self.out(msg, || format!("[recall] {message}"));
    }
}
