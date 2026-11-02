//! Smart-cache score per clip: how likely the user is to look at it soon. Higher scores
//! are filled first and evicted last. All weights live in [`Weights`].
use super::history::History;

/// Half-life of a clip's own interest (opens, plays, hovers, exports): a clip opened once
/// counts half as much a week later, so what was opened recently stays on the SSD.
pub const CLIP_HALF_LIFE_DAYS: f64 = 7.0;

/// Every knob of the score, in one place.
///
/// `score = recency·½^(age/recency_half_life)
///        + browse·depth_fit(age, usual browse depth)
///        + game·(this game's decayed interest ÷ the top game's)
///        + own·(1 − e^(−clip's decayed interest))
///        + favourite·[favourite] + edited·[edited]
///        + just_saved·½^(minutes since saved / just_saved_half_life)`
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Weights {
    /// Half-life of game affinity and browse depth.
    pub memory_half_life_days: f64,
    /// Half-life of each clip's own interest ([`CLIP_HALF_LIFE_DAYS`]).
    pub clip_half_life_days: f64,
    pub recency: f64,
    pub recency_half_life_days: f64,
    /// Clips within the user's usual browse depth get this bonus (fading beyond it).
    pub browse: f64,
    /// Browse depth assumed before any scrolling has been recorded.
    pub default_browse_days: f64,
    pub game: f64,
    pub own: f64,
    pub favourite: f64,
    pub edited: f64,
    /// Fresh saves are usually opened within minutes.
    pub just_saved: f64,
    pub just_saved_half_life_minutes: f64,
    /// Interest added per interaction. A save counts as one open.
    pub open_event: f64,
    pub play_event: f64,
    pub export_event: f64,
    pub hover_event: f64,
    pub favourite_event: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            memory_half_life_days: 14.0,
            clip_half_life_days: CLIP_HALF_LIFE_DAYS,
            recency: 1.0,
            recency_half_life_days: 7.0,
            browse: 0.5,
            default_browse_days: 3.0,
            game: 0.8,
            own: 1.5,
            favourite: 1.0,
            edited: 0.5,
            just_saved: 3.0,
            just_saved_half_life_minutes: 15.0,
            open_event: 1.0,
            play_event: 0.5,
            export_event: 1.5,
            hover_event: 0.05,
            favourite_event: 1.0,
        }
    }
}

impl Weights {
    pub fn memory_half_life_ms(&self) -> f64 {
        self.memory_half_life_days * 86_400_000.0
    }
    pub fn clip_half_life_ms(&self) -> f64 {
        self.clip_half_life_days * 86_400_000.0
    }
}

/// What the scorer needs to know about one listed clip.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ClipFacts {
    /// Lowercase file name (history key).
    pub name: String,
    pub game: String,
    /// Unix ms of the clip's modification time.
    pub mtime_ms: u64,
    pub favourite: bool,
    pub edited: bool,
}

/// Scores for `clips` (same order) at `now_ms`.
pub(super) fn score_all(clips: &[ClipFacts], history: &History, now_ms: u64, w: &Weights) -> Vec<f64> {
    let (half_life, clip_half_life) = (w.memory_half_life_ms(), w.clip_half_life_ms());
    let top_game = history.games.values().map(|g| g.value_at(now_ms, half_life)).fold(0.0, f64::max);
    let depth = match history.browse_days.value_at(now_ms, half_life) {
        d if d > 0.0 => d.max(1.0),
        _ => w.default_browse_days,
    };
    clips
        .iter()
        .map(|clip| {
            let memory = history.clips.get(&clip.name);
            let age_days = now_ms.saturating_sub(clip.mtime_ms) as f64 / 86_400_000.0;
            let recency = w.recency * 0.5f64.powf(age_days / w.recency_half_life_days);
            let browse = w.browse * if age_days <= depth { 1.0 } else { 0.5f64.powf((age_days - depth) / depth) };
            let game_interest = history.games.get(&clip.game).map_or(0.0, |g| g.value_at(now_ms, half_life));
            let game = if top_game > 0.0 { w.game * game_interest / top_game } else { 0.0 };
            let own = w.own * (1.0 - (-memory.map_or(0.0, |m| m.interest.value_at(now_ms, clip_half_life))).exp());
            let favourite = if clip.favourite || memory.is_some_and(|m| m.favourite) { w.favourite } else { 0.0 };
            let edited = if clip.edited { w.edited } else { 0.0 };
            let saved_ms = memory.and_then(|m| m.saved_at_ms).unwrap_or(clip.mtime_ms).max(clip.mtime_ms);
            let minutes = now_ms.saturating_sub(saved_ms) as f64 / 60_000.0;
            let just_saved = w.just_saved * 0.5f64.powf(minutes / w.just_saved_half_life_minutes);
            recency + browse + game + own + favourite + edited + just_saved
        })
        .collect()
}
