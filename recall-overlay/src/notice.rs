//! What a notice shows in the island: a status mark (tone), a small title
//! line and a bold detail line (the game name in Bricolage, else Geist).

use serde::Serialize;
use serde_json::Value;

use crate::reducer::truncate;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// Record red dot: something is being recorded.
    Recording,
    /// Pause bars.
    Paused,
    /// Hollow ring: nothing to record.
    Waiting,
    /// Green dot: saved / done.
    Success,
    /// Error dot: something failed.
    Failure,
    /// Grey dot.
    Neutral,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NoticeView {
    pub tone: Tone,
    /// The small secondary line ("Recording", "Clip saved"); the chord hint's first row.
    pub title: String,
    /// The bold line ("VALORANT"); empty = a one-line notice; the chord hint's second row.
    pub detail: String,
    /// Draw the detail in the display face (it is a game name).
    pub game_font: bool,
    /// The two-step shortcut hint: two equal rows of options.
    pub chord: bool,
}

impl NoticeView {
    pub fn new(tone: Tone, title: &str, detail: impl Into<String>, game_font: bool) -> Self {
        Self { tone, title: title.into(), detail: detail.into(), game_font, chord: false }
    }
}

/// `{"type":"notice","kind":"game|desktop|paused|waiting|encoder","title":S,"detail":S}`;
/// the controller sends "Recording"/<game or "Desktop">, "Recording paused",
/// "Waiting for a game"/"Not recording", "Encoder problem"/"Recorder stopped".
pub fn session_notice(event: &Value) -> NoticeView {
    let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
    let (tone, default_title, game_font) = match kind {
        "game" | "desktop" => (Tone::Recording, "Recording", true),
        "paused" => (Tone::Paused, "Recording paused", false),
        "waiting" => (Tone::Waiting, "Waiting for a game", false),
        "encoder" => (Tone::Failure, "Encoder problem", false),
        _ => (Tone::Neutral, "Recall", false),
    };
    let title = event
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(default_title);
    let detail = event.get("detail").and_then(Value::as_str).unwrap_or("").trim();
    NoticeView::new(tone, &truncate(title, 32), truncate(detail, 40), game_font)
}

/// `kind == "encoder"` (de-duplicated against the engine's own failure).
pub fn is_encoder_notice(event: &Value) -> bool {
    event.get("kind").and_then(Value::as_str) == Some("encoder")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_notice_kinds_map_to_marks_and_faces() {
        let game = session_notice(&json!({"type":"notice","kind":"game","title":"Recording","detail":"VALORANT"}));
        assert_eq!(game, NoticeView::new(Tone::Recording, "Recording", "VALORANT", true));
        let desktop = session_notice(&json!({"kind":"desktop","title":"Recording","detail":"Desktop"}));
        assert_eq!((desktop.tone, desktop.game_font), (Tone::Recording, true));
        assert_eq!(session_notice(&json!({"kind":"paused"})).title, "Recording paused");
        let waiting = session_notice(&json!({"kind":"waiting","title":"Waiting for a game","detail":"Not recording"}));
        assert_eq!((waiting.tone, waiting.detail.as_str(), waiting.game_font), (Tone::Waiting, "Not recording", false));
        let encoder = session_notice(&json!({"kind":"encoder","title":"Encoder problem","detail":"Recorder stopped"}));
        assert_eq!((encoder.tone, encoder.game_font), (Tone::Failure, false));
        assert!(is_encoder_notice(&json!({"kind":"encoder"})) && !is_encoder_notice(&json!({"kind":"game"})));
    }
}
