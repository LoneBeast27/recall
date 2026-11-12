//! `--smoke-state FIXTURE.json`: drives the island without a controller.
//!
//! ```json
//! {"events":[{"mod":"session","event":{...}}, {"at_ms":3500,"mod":"engine","event":{...}}],
//!  "stats":{"fps":144,"low1":98,"gpu":71,"gpu_temp":64,"cpu":23},
//!  "hold":false, "exit_ms":9000}
//! ```
//! Events without `at_ms` apply at start; `stats` replaces the live sources;
//! `hold` freezes notice expiry; `exit_ms` quits the overlay by itself.
//! `"editor": true` places the island the Dynamic way (with `--corner
//! dynamic`), so the island editor can run: an `{"mod":"session","event":
//! {"type":"island_editor"}}` event toggles it as the shortcut does.

use serde_json::Value;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::reducer::Reducer;
use crate::stats::StatsSnapshot;

pub struct Smoke {
    timeline: Vec<(Duration, Value)>,
    next: usize,
    pub stats: Option<StatsSnapshot>,
    pub hold: bool,
    pub exit_after: Option<Duration>,
    /// Dynamic placement and the island editor are allowed.
    pub editor: bool,
    /// Editor shortcut events due and not yet taken by the app.
    pub toggles: u32,
}

impl Smoke {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
        Ok(Self::from_value(&value))
    }

    pub fn from_value(value: &Value) -> Self {
        let events: Vec<Value> = match value.as_array().or_else(|| value.get("events").and_then(Value::as_array)) {
            Some(list) => list.clone(),
            None => vec![value.clone()],
        };
        let mut timeline: Vec<(Duration, Value)> = events
            .into_iter()
            .map(|event| {
                let at = event.get("at_ms").and_then(Value::as_u64).unwrap_or(0);
                (Duration::from_millis(at), event)
            })
            .collect();
        timeline.sort_by_key(|(at, _)| *at);
        let millis = |name: &str| value.get(name).and_then(Value::as_u64).map(Duration::from_millis);
        Self {
            timeline,
            next: 0,
            stats: value.get("stats").filter(|s| s.is_object()).map(StatsSnapshot::from_fixture),
            hold: value.get("hold").and_then(Value::as_bool).unwrap_or(false),
            exit_after: millis("exit_ms"),
            editor: value.get("editor").and_then(Value::as_bool).unwrap_or(false),
            toggles: 0,
        }
    }

    /// Applies every event due by `elapsed`; true when any applied.
    pub fn apply_due(&mut self, reducer: &mut Reducer, elapsed: Duration, now: Instant) -> bool {
        let mut changed = false;
        while let Some((at, event)) = self.timeline.get(self.next) {
            if *at > elapsed {
                break;
            }
            changed |= if crate::app::editor::is_toggle(event) {
                self.toggles += 1;
                false
            } else if event.get("mod").is_some() {
                reducer.apply_envelope(event, now)
            } else {
                reducer.apply_engine_event(event, now)
            };
            self.next += 1;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn timeline_applies_in_time_order() {
        let fixture = json!({"events":[
            {"at_ms":2000,"mod":"session","event":{"type":"notice","kind":"game","detail":"VALORANT"}},
            {"mod":"session","event":{"type":"status","state":"recording","overlay_stats":true}}],
            "stats":{"fps":144,"gpu":70},"exit_ms":5000});
        let mut smoke = Smoke::from_value(&fixture);
        assert_eq!(smoke.stats.as_ref().and_then(|s| s.fps), Some(144));
        assert_eq!(smoke.exit_after, Some(Duration::from_secs(5)));
        let (mut reducer, now) = (Reducer::new(), Instant::now());
        assert!(smoke.apply_due(&mut reducer, Duration::ZERO, now));
        assert!(reducer.session().overlay_stats && !reducer.view().visible);
        assert!(!smoke.apply_due(&mut reducer, Duration::from_millis(1999), now));
        assert!(smoke.apply_due(&mut reducer, Duration::from_millis(2000), now));
        assert!(reducer.view().visible);
    }
}
