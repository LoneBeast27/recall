//! The recorded sources as the Audio page edits them: the controller's
//! `recording.audio_sources` (a list of `{id, kind, device, exe, name, enabled, gain_db}` in
//! track order). Every change writes the whole list back with `set_config`; the recorder
//! restarts like it does for a format change. Pure: no devices, no UI.
use serde_json::{Value, json};

/// Same cap as the controller.
pub const SOURCES_MAX: usize = 8;
pub const GAIN_MIN_DB: f32 = -60.0;
pub const GAIN_MAX_DB: f32 = 12.0;
/// Longest name the controller takes.
pub const NAME_MAX: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The recorded game's process; on the desktop, an output device.
    Game,
    /// An output device by loopback.
    Output,
    /// A microphone or line-in.
    Input,
    /// One app's sound, by exe.
    App,
}

impl Kind {
    pub fn token(self) -> &'static str {
        match self {
            Kind::Game => "game",
            Kind::Output => "output",
            Kind::Input => "input",
            Kind::App => "app",
        }
    }

    fn from_token(token: &str) -> Option<Kind> {
        Some(match token {
            "game" => Kind::Game,
            "output" => Kind::Output,
            "input" => Kind::Input,
            "app" => Kind::App,
            _ => return None,
        })
    }

    /// The name a new source of this kind starts with.
    pub fn default_name(self) -> &'static str {
        match self {
            Kind::Game => "Game",
            Kind::Output => "Desktop audio",
            Kind::Input => "Microphone",
            Kind::App => "App",
        }
    }

    /// The device list a source picks from: inputs for an input, outputs for the rest.
    pub fn uses_inputs(self) -> bool {
        self == Kind::Input
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub id: String,
    pub kind: Kind,
    /// Endpoint name ("" = the Windows default).
    pub device: String,
    /// App image name, e.g. `player.exe` (app sources only).
    pub exe: String,
    pub name: String,
    pub enabled: bool,
    pub gain_db: f32,
    /// Whether the mic-mute hotkey silences this input: `true` a microphone, `false` a line-in
    /// or capture card (muting the mic leaves its sound, once it names a device); `None` (a
    /// config written before the flag, the default mic) = muted, as the recorder does.
    /// "+ Add source" writes it. Other kinds ignore it.
    pub mic_mute: Option<bool>,
}

/// What Alt+M does to `source` now, as the recorder decides it: an input is muted unless it is a
/// line-in (`mic_mute = false`) on a named device. Without a flag it is muted whatever Windows
/// calls its endpoint (R3-deep-audio-1); on the Windows default it is muted whatever the flag
/// says, as that is normally the microphone (R4-deep-audio-1).
pub fn mutes(source: &Source) -> bool {
    source.kind == Kind::Input && (source.mic_mute != Some(false) || source.device.trim().is_empty())
}

/// The note a line-in on the Windows default carries (strip and device menu).
pub const LINE_IN_ON_DEFAULT: &str = "Follows the mic hotkey until you pick a device";

/// The warning line under a strip's device button, from its label: a device that is gone, two
/// endpoints sharing its name (neither is recorded), or a line-in still on the Windows default.
pub fn strip_note(label: &str, source: &Source) -> Option<&'static str> {
    if label.ends_with("(not connected)") {
        Some("Not connected")
    } else if label.ends_with("rename one in Windows)") {
        Some("Two devices share this name")
    } else if line_in_on_default(source) {
        Some(LINE_IN_ON_DEFAULT)
    } else {
        None
    }
}

/// A line-in on the Windows default: its strip says it follows the mic hotkey until a device is
/// picked (R4-deep-audio-1).
pub fn line_in_on_default(source: &Source) -> bool {
    source.kind == Kind::Input && source.mic_mute == Some(false) && source.device.trim().is_empty()
}

/// A hint for the strip menu, never a decision: Windows reports the endpoint `source` opens (its
/// device, else the Windows default) as a line-level input (`line_inputs`, from `endpoint_form`).
/// Unknown, S/PDIF and HDMI endpoints get no hint (R4-deep-audio-2), nor do lists not read yet or
/// a device not connected.
pub fn windows_says_line_level(source: &Source, lists: Option<&crate::services::audio_devices::DeviceLists>) -> bool {
    let Some(lists) = lists.filter(|_| source.kind == Kind::Input) else { return false };
    if source.device.trim().is_empty() {
        return lists.default_input_is_line == Some(true);
    }
    match crate::services::meters::match_endpoint(&lists.inputs, &source.device) {
        Some(i) => lists.line_inputs.iter().any(|m| m.trim() == lists.inputs[i].trim()),
        None => false,
    }
}

/// The saved list (`recording.audio_sources`), skipping entries the controller would refuse and,
/// like the engine, a repeated id (case-insensitive): a strip whose track is never recorded
/// would meter live sound for nothing.
pub fn from_config(value: Option<&Value>) -> Vec<Source> {
    let Some(list) = value.and_then(Value::as_array) else { return Vec::new() };
    let mut ids = std::collections::HashSet::new();
    list.iter()
        .filter_map(|v| {
            let text = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
            Some(Source {
                id: Some(text("id")).filter(|id| !id.trim().is_empty())?,
                kind: Kind::from_token(&text("kind"))?,
                device: text("device"),
                exe: text("exe"),
                name: text("name"),
                enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                gain_db: v.get("gain_db").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                mic_mute: v.get("mic_mute").and_then(Value::as_bool),
            })
        })
        .filter(|s| ids.insert(s.id.trim().to_lowercase()))
        .collect()
}

pub fn to_config(list: &[Source]) -> Value {
    Value::Array(
        list.iter()
            .map(|s| {
                let mut entry = json!({ "id": s.id, "kind": s.kind.token(), "device": s.device, "exe": s.exe, "name": s.name, "enabled": s.enabled, "gain_db": s.gain_db });
                if let Some(on) = s.mic_mute {
                    entry["mic_mute"] = Value::Bool(on);
                }
                entry
            })
            .collect(),
    )
}

/// An id not in `list`: `<kind>-<n>`.
fn fresh_id(list: &[Source], kind: Kind) -> String {
    (1..).map(|n| format!("{}-{n}", kind.token())).find(|id| !list.iter().any(|s| s.id.eq_ignore_ascii_case(id))).expect("an unused id")
}

/// A name not in `list`: the base, else "<base> 2", "<base> 3", ...
fn fresh_name(list: &[Source], base: &str) -> String {
    let taken = |name: &str| list.iter().any(|s| s.name.trim().eq_ignore_ascii_case(name));
    if !taken(base) {
        return base.to_owned();
    }
    (2..).map(|n| format!("{base} {n}")).find(|name| !taken(name)).expect("an unused name")
}

/// Kinds "+ Add source" offers: every kind but a second game.
pub fn addable(list: &[Source]) -> Vec<Kind> {
    if list.len() >= SOURCES_MAX {
        return Vec::new();
    }
    let game = list.iter().any(|s| s.kind == Kind::Game);
    [Kind::Input, Kind::Output, Kind::App, Kind::Game].into_iter().filter(|k| *k != Kind::Game || !game).collect()
}

/// `list` with a new source of `kind` at the end, on the Windows default (an app source takes
/// `exe` and its display name); `None` when the list is full or a second game would be added.
pub fn with_new(list: &[Source], kind: Kind, exe: &str, app_name: &str) -> Option<Vec<Source>> {
    if !addable(list).contains(&kind) || (kind == Kind::App && exe.trim().is_empty()) {
        return None;
    }
    let base = if kind == Kind::App && !app_name.trim().is_empty() { clip_name(app_name) } else { kind.default_name().to_owned() };
    let mut out = list.to_vec();
    out.push(Source { id: fresh_id(list, kind), kind, device: String::new(), exe: exe.trim().to_owned(), name: fresh_name(list, &base), enabled: true, gain_db: 0.0, mic_mute: None });
    Some(out)
}

/// "+ Add source" → "Microphone" (`mic`) or "Line-in or capture card": a new input that says
/// whether Alt+M mutes it, whatever its endpoint's form factor. A line-in starts on the Windows
/// default, so Alt+M still mutes it until its device is picked (`mutes`).
pub fn with_new_input(list: &[Source], mic: bool) -> Option<Vec<Source>> {
    let mut out = with_new(list, Kind::Input, "", "")?;
    let added = out.last_mut().expect("with_new adds one");
    added.mic_mute = Some(mic);
    if !mic {
        added.name = fresh_name(list, "Line-in");
    }
    Some(out)
}

/// `list` with a removed `source` back at `index` (Undo); `None` when it cannot come back (the
/// list is full, or it is a game and another game was added). A source added since may have
/// taken its freed id: then it comes back under a fresh one.
pub fn restored(list: &[Source], index: usize, mut source: Source) -> Option<Vec<Source>> {
    if list.len() >= SOURCES_MAX || (source.kind == Kind::Game && list.iter().any(|s| s.kind == Kind::Game)) {
        return None;
    }
    if list.iter().any(|s| s.id.eq_ignore_ascii_case(&source.id)) {
        source.id = fresh_id(list, source.kind);
    }
    let mut out = list.to_vec();
    out.insert(index.min(out.len()), source);
    Some(out)
}

/// A name the controller takes: one line, at most `NAME_MAX` characters, not blank.
pub fn clip_name(name: &str) -> String {
    let one_line: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    one_line.trim().chars().take(NAME_MAX).collect::<String>().trim().to_owned()
}

/// `list` with the source `id` moved to slot `to` (clamped to the end); the same list when
/// `id` is not in it. The list order is the clips' track order.
pub fn moved(list: &[Source], id: &str, to: usize) -> Vec<Source> {
    let mut out = list.to_vec();
    if let Some(from) = out.iter().position(|s| s.id == id) {
        let source = out.remove(from);
        out.insert(to.min(out.len()), source);
    }
    out
}

/// The slot a dragged strip lands in when the pointer is at window x `x`: `count` strips,
/// `pitch` apart, the first one's left edge at `left`.
pub fn drop_slot(x: f32, left: f32, pitch: f32, count: usize) -> usize {
    if count == 0 || pitch <= 0.0 {
        return 0;
    }
    ((x - left) / pitch).floor().clamp(0.0, (count - 1) as f32) as usize
}

/// Escape cancels a strip drag: only a bare Escape, and only while a drag is on.
/// Any other press, or Escape with no drag, goes on to the other key handlers.
pub fn escape_cancels_drag(key: &str, any_modifier: bool, dragging: bool) -> bool {
    dragging && key == "escape" && !any_modifier
}

/// Gain for a fader position (0 = bottom, 1 = top), in 0.5 dB steps.
pub fn gain_at(fraction: f32) -> f32 {
    let db = GAIN_MIN_DB + fraction.clamp(0.0, 1.0) * (GAIN_MAX_DB - GAIN_MIN_DB);
    (db * 2.0).round() / 2.0
}

/// Fader position (0..1) of a gain.
pub fn fader_fraction(gain_db: f32) -> f32 {
    ((gain_db.clamp(GAIN_MIN_DB, GAIN_MAX_DB) - GAIN_MIN_DB) / (GAIN_MAX_DB - GAIN_MIN_DB)).clamp(0.0, 1.0)
}

/// How long a let-go fader keeps its gain on screen while the save comes back from the recorder.
pub const PENDING_GAIN_FOR: std::time::Duration = std::time::Duration::from_secs(3);

/// The gain a strip shows: the fader while it is dragged, else the gain just let go (until the
/// saved list carries it, at most `PENDING_GAIN_FOR` after release), else the saved gain.
/// Without the middle step the fader and meter snap back to the old gain for the round trip.
pub fn shown_gain(dragging: Option<f32>, pending: Option<(f32, std::time::Duration)>, saved: f32) -> f32 {
    if let Some(gain) = dragging {
        return gain;
    }
    match pending {
        Some((gain, since)) if since < PENDING_GAIN_FOR && (gain - saved).abs() > 0.01 => gain,
        _ => saved,
    }
}

/// "+3.5 dB", "0 dB", "-60 dB".
pub fn gain_label(gain_db: f32) -> String {
    if gain_db.abs() < 0.05 {
        "0 dB".to_owned()
    } else if gain_db.fract().abs() < 0.05 {
        format!("{:+.0} dB", gain_db)
    } else {
        format!("{:+.1} dB", gain_db)
    }
}

#[cfg(test)]
#[path = "audio_sources_tests.rs"]
mod tests;
