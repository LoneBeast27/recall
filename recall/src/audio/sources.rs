//! The recorded sources: `RECALL_AUDIO_SOURCES` (the controller's `recording.audio_sources`,
//! enabled ones, as JSON) in track order. Each source is one track named after it:
//! - `game`: the game's process tree (`RECALL_AUDIO_GAME_PID`); on the desktop, loopback
//!   of `device` ("" = the Windows default output).
//! - `output`: loopback of an output device; `input`: a microphone or line-in.
//! - `app`: an app's process tree by `exe` (`apps.rs`).
//!
//! Without `RECALL_AUDIO_SOURCES` (an engine started by hand or by an older controller)
//! the old variables still work: Game (`RECALL_AUDIO_GAME`), Mic (`RECALL_AUDIO_MIC`, none
//! with `RECALL_AUDIO_MIC_OFF=1`), a voice app (`RECALL_AUDIO_VOICE_EXE`) and
//! `RECALL_AUDIO_APPS`.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::apps::{self, AppSource};
use super::TrackRing;

pub const SOURCES_ENV: &str = "RECALL_AUDIO_SOURCES";
/// Same cap as the controller.
pub const SOURCES_MAX: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Game,
    Output,
    Input,
    App,
}

/// One source as the controller sends it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SourceSpec {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub device: String,
    /// The endpoint id (`IMMDevice::GetId`) the Audio page saved with `device` when it was
    /// picked; "" = none (a config written before the id, or a hand-made one), and so is a
    /// malformed one (`endpoints::id_ok`). Only decides whether a line-in may skip Alt+M (only
    /// with an id, `endpoints::opened_own`), never which endpoint opens.
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub exe: String,
    pub name: String,
    #[serde(default)]
    pub gain_db: f32,
    /// Whether the mic-mute hotkey silences this input: `true` a microphone, `false` a line-in
    /// or capture card; absent = muted (every config written before the flag, the default mic).
    /// Ignored for other kinds.
    #[serde(default)]
    pub mic_mute: Option<bool>,
    #[serde(default)]
    pub communications: bool,
}

impl SourceSpec {
    pub fn kind(&self) -> Option<Kind> {
        Some(match self.kind.as_str() {
            "game" => Kind::Game,
            "output" => Kind::Output,
            "input" => Kind::Input,
            "app" => Kind::App,
            _ => return None,
        })
    }

    pub fn uses_communications(&self) -> bool {
        self.kind() == Some(Kind::Output) && self.device.trim().is_empty() && self.communications
    }

    /// Linear factor for `gain_db`, clamped to the controller's -60..+12 dB.
    pub fn gain(&self) -> f32 {
        gain_of(self.gain_db)
    }
}

/// Linear factor for a gain in dB, clamped to the controller's -60..+12 dB (NaN = 0 dB).
pub fn gain_of(gain_db: f32) -> f32 {
    let db = if gain_db.is_finite() { gain_db.clamp(-60.0, 12.0) } else { 0.0 };
    10f32.powf(db / 20.0)
}

fn spec(id: &str, kind: &str, name: &str, device: &str, exe: &str) -> SourceSpec {
    SourceSpec { communications: false, device_id: String::new(), id: id.into(), kind: kind.into(), device: device.trim().into(), exe: exe.trim().into(), name: name.into(), gain_db: 0.0, mic_mute: None }
}

/// The sources in `json`, with every bad entry skipped and logged: one broken entry (a
/// duplicate id from a copy-paste, a typo in its kind, a ninth source) must not take the other
/// tracks, the game's included, out of every clip. Only unreadable JSON is an error.
pub fn parse(json: &str) -> Result<Vec<SourceSpec>, String> {
    let (list, skipped) = parse_entries(json)?;
    for why in skipped {
        eprintln!("[recall] {SOURCES_ENV}: skipped {why}");
    }
    Ok(list)
}

/// [`parse`]'s list and why each skipped entry was skipped.
pub(super) fn parse_entries(json: &str) -> Result<(Vec<SourceSpec>, Vec<String>), String> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(json).map_err(|e| format!("{SOURCES_ENV}: {e}"))?;
    let mut ids = std::collections::HashSet::new();
    let (mut list, mut skipped) = (Vec::new(), Vec::new());
    for (n, entry) in entries.into_iter().enumerate() {
        match entry_problem(serde_json::from_value::<SourceSpec>(entry), &mut ids) {
            Ok(s) if list.len() >= SOURCES_MAX => skipped.push(format!("source '{}': at most {SOURCES_MAX} sources", s.id)),
            Ok(s) => list.push(s),
            Err(why) => skipped.push(format!("entry {}: {why}", n + 1)),
        }
    }
    Ok((list, skipped))
}

/// One entry, or what is wrong with it; `ids` holds the ids taken by earlier entries.
fn entry_problem(entry: Result<SourceSpec, serde_json::Error>, ids: &mut std::collections::HashSet<String>) -> Result<SourceSpec, String> {
    let s = entry.map_err(|e| e.to_string())?;
    if s.id.trim().is_empty() || ids.contains(&s.id.trim().to_lowercase()) {
        return Err(format!("source id '{}' is empty or used twice", s.id));
    }
    if s.kind().is_none() {
        return Err(format!("source '{}' has unknown kind '{}'", s.id, s.kind));
    }
    if s.kind() == Some(Kind::App) && (s.exe.trim().is_empty() || s.exe.contains(['\\', '/', ':'])) {
        return Err(format!("source '{}' needs an exe file name", s.id));
    }
    ids.insert(s.id.trim().to_lowercase());
    Ok(s)
}

/// The sources for this engine run (`get` reads the environment; injected in tests).
pub fn resolve(get: impl Fn(&str) -> Option<String>) -> Result<Vec<SourceSpec>, String> {
    if let Some(json) = get(SOURCES_ENV) {
        return parse(&json);
    }
    let set = |key: &str| get(key).filter(|v| !v.trim().is_empty());
    let mut list = vec![spec("game", "game", "Game", &set("RECALL_AUDIO_GAME").unwrap_or_default(), "")];
    if set("RECALL_AUDIO_MIC_OFF").as_deref().map(str::trim) != Some("1") {
        list.push(spec("mic", "input", "Microphone", &set("RECALL_AUDIO_MIC").unwrap_or_default(), ""));
    }
    if let Some(exe) = set("RECALL_AUDIO_VOICE_EXE") {
        list.push(spec("voice", "app", "Voice chat", "", &exe));
    }
    if let Some(json) = set(apps::APPS_ENV) {
        for (n, app) in apps::parse(&json)?.into_iter().enumerate() {
            list.push(spec(&format!("app-{}", n + 1), "app", &app.track_name(), "", &app.exe));
        }
    }
    list.truncate(SOURCES_MAX);
    Ok(list)
}

pub fn from_env() -> Result<Vec<SourceSpec>, String> {
    resolve_for_start(|key| std::env::var(key).ok())
}

/// [`resolve`] with the `set_source` flags and ids read from stdin before the tracks start
/// ([`stage_early`]) laid over it, so they are in force from the first packet.
pub fn resolve_for_start(get: impl Fn(&str) -> Option<String>) -> Result<Vec<SourceSpec>, String> {
    let mut list = resolve(get)?;
    let staged = lock_early().staged.clone();
    // A source retired before the listing (removed or disabled during the spawn, F-3) gets no
    // track at all: an app or process-tree track never starts held, so it would record.
    let retired: Vec<String> = staged.iter().flatten().filter(|s| s.3).map(|s| s.0.trim().to_owned()).collect();
    list.retain(|s| !retired.iter().any(|r| *r == s.id.trim()));
    for (id, mic_mute, device_id, _) in staged.into_iter().flatten() {
        for spec in list.iter_mut().filter(|s| s.id.trim() == id.trim()) {
            if let Some(on) = mic_mute {
                spec.mic_mute = Some(on);
            }
            if let Some(device_id) = &device_id {
                spec.device_id = device_id.clone(); // a malformed id is dropped at open, as from the env
            }
        }
    }
    Ok(list)
}

/// `set_source` flags and ids read before the tracks start, in order; None once they started.
type Staged = Vec<(String, Option<bool>, Option<String>, bool)>;

/// The start of the stdin protocol, under one lock: the lines staged before the tracks are
/// listed, and whether the controller's `sync` was read (the startup hold ends on both).
struct Early {
    /// `set_source` flags and ids read before the tracks start, in order; None once listed.
    staged: Option<Staged>,
    /// `sync` was read: every line the controller queued before it was applied, in order.
    synced: bool,
    /// The reader ended (`hold_for_quit`): nothing is released again, and a device track
    /// listed from now on starts retired.
    quitting: bool,
}

static EARLY: Mutex<Early> = Mutex::new(Early { staged: Some(Vec::new()), synced: false, quitting: false });

fn lock_early() -> std::sync::MutexGuard<'static, Early> {
    EARLY.lock().unwrap_or_else(|p| p.into_inner())
}

/// A `set_source` read from stdin (review R2-audio-r9-2): before the tracks start, its mic-mute
/// flag and device id are noted for [`resolve_for_start`]; once they started ([`close_staging`])
/// it goes to the live track at once, so a line read is a line applied (the startup hold,
/// merge-gate P1-1, ends on that). `serve` applies it again later (the same values).
pub fn stage_early(id: &str, mic_mute: Option<bool>, device_id: Option<&str>) {
    if mic_mute.is_none() && device_id.is_none() {
        return;
    }
    stage(id, LiveChange { mic_mute, device_id, ..LiveChange::default() });
}

/// A `set_source retire` read from stdin: staged before the tracks are listed (they start held,
/// and the close retires the track), else applied to the live track at once.
pub fn stage_retire(id: &str) {
    stage(id, LiveChange { retire: true, ..LiveChange::default() });
}

fn stage(id: &str, change: LiveChange) {
    if let Some(staged) = lock_early().staged.as_mut() {
        return staged.push((id.to_owned(), change.mic_mute, change.device_id.map(str::to_owned), change.retire));
    }
    set_live(id, change);
}

/// The tracks started and are listed (`note_live`): every line staged so far goes to its live
/// track (one read after the list was resolved included), and later ones go there at once.
pub(super) fn close_staging() {
    close_staging_with(|id, change| {
        set_live(id, change);
    });
}

/// [`close_staging`] with the applier passed in (tests). The `EARLY` lock is held across every
/// apply (merge-gate F2): a newer `set_source` read meanwhile waits in [`stage_early`] and goes
/// live after the older staged ones, never before them (lock order EARLY, then STATES).
fn close_staging_with(mut apply: impl FnMut(&str, LiveChange)) {
    let mut early = lock_early(); // held to the end: the safe state
    for (id, mic_mute, device_id, retire) in early.staged.take().into_iter().flatten() {
        apply(&id, LiveChange { mic_mute, device_id: device_id.as_deref(), retire, ..LiveChange::default() });
    }
    if early.synced && !early.quitting {
        release_startup_holds(); // `sync` came first: every line before it is now on its track
    }
}

/// The controller's `sync` was read (the sync handshake): every line it queued before it was
/// applied, in order (one writer, one FIFO pipe, one reader). The device tracks record once
/// they are also listed ([`close_staging`]); whichever of the two comes second releases them,
/// under the `EARLY` lock, after the staged lines went to their tracks. Nothing else releases
/// them: no `sync` (an old controller, a read error, the end of stdin) keeps them held.
pub fn sync_read() {
    let mut early = lock_early();
    early.synced = true;
    if early.staged.is_none() && !early.quitting {
        release_startup_holds();
    }
}

/// Whether the startup hold has ended (`sync` read and every track listed).
pub fn startup_released() -> bool {
    let early = lock_early();
    early.synced && early.staged.is_none()
}

/// The reader ended (the controller's `quit`, the end of stdin or a read error: no line reaches
/// this engine after it, `engine_stdin::reader_end`): every listed device
/// track is retired, so from its next packet it stores silence until the exit, the long
/// recording's finalise included. The game's process tree and app tracks keep recording: no
/// line can change what they capture. Never undone (the engine is ending).
pub fn hold_for_quit() {
    let mut early = lock_early(); // lock order EARLY, then STATES
    early.quitting = true; // a track listed after this starts retired (`note_live`), N1
    if let Ok(states) = STATES.lock() {
        for (_, _, state) in states.iter() {
            if let State::Live(ring) = state {
                if is_device(ring) {
                    ring.retire();
                }
            }
        }
    }
    eprintln!("[recall] stdin: quit; every device audio track stores silence until the exit");
}

/// A track a line can change (an input or an output endpoint); not the game's process tree or
/// an app track.
fn is_device(ring: &TrackRing) -> bool {
    ring.source != crate::edit_model::SOURCE_GAME_PROCESS && !ring.source.starts_with(crate::edit_model::APP_SOURCE_PREFIX)
}

/// The startup hold of every listed track ends (called with `EARLY` held; lock order EARLY,
/// then STATES).
fn release_startup_holds() {
    if let Ok(states) = STATES.lock() {
        for (_, _, state) in states.iter() {
            if let State::Live(ring) = state {
                ring.release_startup_hold();
            }
        }
    }
    eprintln!("[recall] stdin: the controller's settings are applied; the audio tracks record");
    crate::protocol::emit(&crate::protocol::Outbound::InputsReleased);
}

/// Tests: [`close_staging_with`], then staging open again (every staging test holds one lock).
#[cfg(test)]
pub(crate) fn close_staging_for_test(apply: impl FnMut(&str, LiveChange)) {
    close_staging_with(apply);
}

#[cfg(test)]
pub(crate) fn reopen_staging_for_test() {
    *lock_early() = Early { staged: Some(Vec::new()), synced: false, quitting: false };
}

/// Tests: list `ring` as the live track of input source `id` (as `audio::start` does).
#[cfg(test)]
pub(crate) fn register_for_test(id: &str, ring: &Arc<TrackRing>) {
    note_live(&spec(id, "input", id, "", ""), ring);
}

/// A change the Audio page makes to a running source: gain, name, mic-mute flag, and the
/// endpoint id saved with its device (P1-B; the track reopens on it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveChange<'a> {
    pub gain_db: Option<f32>,
    pub name: Option<&'a str>,
    pub mic_mute: Option<bool>,
    pub device_id: Option<&'a str>,
    /// Retire the track (`TrackRing::retire`): never undone.
    pub retire: bool,
}

/// Apply `change` to the live track (no engine restart, so the replay buffer stays). False
/// when no live track has that id.
pub fn set_live(id: &str, change: LiveChange) -> bool {
    let LiveChange { gain_db, name, mic_mute, device_id, retire } = change;
    let Ok(mut states) = STATES.lock() else { return false };
    let mut found = false;
    for (sid, sname, state) in states.iter_mut() {
        if sid.trim() != id.trim() {
            continue;
        }
        if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
            *sname = name.to_owned();
            if let State::Live(ring) = state {
                ring.set_title(name);
            }
        }
        if let (Some(db), State::Live(ring)) = (gain_db, &*state) {
            ring.set_gain_db(db);
        }
        // The id first (F-2): the stream on the old device holds before the flag can unmute it.
        if let (Some(device_id), State::Live(ring)) = (device_id, &*state) {
            // A malformed id counts as none (`endpoints::id_ok`), as at spawn.
            ring.live.set_saved_id(if super::endpoints::id_ok(device_id) { device_id } else { "" });
        }
        if let (Some(on), State::Live(ring)) = (mic_mute, &*state) {
            ring.set_mic_mute(on);
        }
        if let (true, State::Live(ring)) = (retire, &*state) {
            ring.retire(); // silence from its next packet, its stream closed
        }
        found |= matches!(state, State::Live(_));
    }
    found
}

/// What one source opened, for the engine status (`audio` in `status`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Opened {
    pub id: String,
    pub name: String,
    /// The endpoint or process captured ("… (Windows default)" for a source with no device,
    /// "<name> (not connected; waiting)" for a named device not there yet), or why there is no
    /// track.
    pub opened: String,
    pub live: bool,
    /// The endpoint id an unsure sibling offers as the source's own (`hold.rs`).
    pub adopt_id: Option<String>,
    /// The track stores silence whatever Alt+M says (`TrackRing::held_now`).
    pub held: bool,
    /// Alt+M silences it right now (`TrackRing::mutes_now`): the meter's "Muted" is this.
    pub mutes: bool,
    /// Retired: recorded as nothing until the next engine (`TrackRing::retire`).
    pub retired: bool,
}

enum State {
    Live(Arc<TrackRing>),
    Failed(String),
}

static STATES: Mutex<Vec<(String, String, State)>> = Mutex::new(Vec::new());

pub(super) fn note_live(spec: &SourceSpec, ring: &Arc<TrackRing>) {
    let early = lock_early(); // lock order EARLY, then STATES: no `hold_for_quit` in between
    if early.quitting && is_device(ring) {
        ring.retire(); // listed after the reader ended: never released (N1)
    }
    if let Ok(mut states) = STATES.lock() {
        states.push((spec.id.clone(), spec.name.clone(), State::Live(Arc::clone(ring))));
    }
}

pub(super) fn note_failed(spec: &SourceSpec, why: String) {
    if let Ok(mut states) = STATES.lock() {
        states.push((spec.id.clone(), spec.name.clone(), State::Failed(why)));
    }
}

/// Every source's current capture, in list order.
pub fn opened() -> Vec<Opened> {
    let Ok(states) = STATES.lock() else { return Vec::new() };
    states
        .iter()
        .map(|(id, name, state)| match state {
            State::Live(ring) => Opened { id: id.clone(), name: name.clone(), opened: ring.endpoint(), live: true, adopt_id: ring.live.adopted(), held: ring.held_now(), mutes: ring.mutes_now(), retired: ring.retired() },
            State::Failed(why) => Opened { id: id.clone(), name: name.clone(), opened: why.clone(), live: false, adopt_id: None, held: false, mutes: false, retired: false },
        })
        .collect()
}

/// The app sources as `apps.rs` takes them (track name, ring id and gain from the source).
pub(super) fn app_sources(list: &[SourceSpec]) -> Vec<AppSource> {
    list.iter()
        .filter(|s| s.kind() == Some(Kind::App))
        .map(|s| AppSource { exe: s.exe.trim().to_owned(), label: s.name.clone(), id: s.id.clone(), gain_db: s.gain_db })
        .collect()
}

/// `rings` in the order of `list` (app tracks start together, after the others).
pub(super) fn in_list_order(list: &[SourceSpec], mut rings: Vec<Arc<TrackRing>>) -> Vec<Arc<TrackRing>> {
    rings.sort_by_key(|r| list.iter().position(|s| s.id.trim() == r.name).unwrap_or(usize::MAX));
    rings
}

#[cfg(test)]
#[path = "sources_tests.rs"]
mod tests;
