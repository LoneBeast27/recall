//! The update's progress bar. Measured where Recall can count (GitHub answers received, Setup
//! bytes downloaded); estimated from past updates (`update_timings.json`, made by
//! `scripts/update-timings.py`) only for waits it cannot count while they run: GitHub's first
//! answer, the checksum pass and opening Setup. An estimated stretch fills toward 90 % of its
//! part and stops there until the real event, so the bar is never full early and never moves
//! back (`View::fraction` only grows).
use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

/// What the update is doing now, in order (a step never goes back).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// "Check now": GitHub requests, counted.
    Checking,
    /// The Setup download asked for, no byte yet.
    Connecting,
    /// Bytes arriving, counted.
    Downloading,
    /// SHA-256 of the saved Setup.
    Verifying,
    /// Setup handed to the shell; Recall quits once it opens.
    Opening,
}

/// Medians (and Setup's spread) from past updates, in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Timings {
    pub request: f64,
    pub connect: f64,
    pub mbps: f64,
    pub verify: f64,
    pub open: f64,
    /// Setup's own update, start to "Recall reopened" (median, p90, samples).
    pub setup: (f64, f64, u32),
}

impl Timings {
    /// Parse the shipped file; a missing phase keeps the safe default.
    pub fn parse(json: &str) -> Self {
        let doc: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
        let phase = |name: &str, key: &str, fallback: f64| {
            doc["phases"][name][key].as_f64().filter(|v| v.is_finite() && *v > 0.0).unwrap_or(fallback)
        };
        Self {
            request: phase("check_request_s", "median", 0.8),
            connect: phase("connect_s", "median", 1.0),
            mbps: phase("download_mbps", "median", 20.0),
            verify: phase("verify_s", "median", 0.3),
            open: phase("open_setup_s", "median", 0.6),
            setup: (
                phase("setup_update_s", "median", 8.0),
                phase("setup_update_s", "p90", 12.0),
                doc["phases"]["setup_update_s"]["n"].as_u64().unwrap_or(0) as u32,
            ),
        }
    }
}

pub fn timings() -> &'static Timings {
    static T: OnceLock<Timings> = OnceLock::new();
    T.get_or_init(|| Timings::parse(include_str!("update_timings.json")))
}

/// An estimated stretch: 45 % of its part at the median time, never past 90 %.
pub(crate) fn estimate(elapsed: Duration, median: f64) -> f64 {
    let half_life = median.max(0.05);
    0.9 * (1.0 - 0.5f64.powf(elapsed.as_secs_f64() / half_life))
}

/// The most the bar shows before the work is really over (the app closes or the card goes).
const NEVER_FULL: f64 = 0.99;

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub step: Step,
    /// 0..=0.99, never smaller than the last view's.
    pub fraction: f32,
    pub label: String,
}

struct State {
    step: Step,
    since: Instant,
    answered: u32,
    planned: u32,
    bytes: u64,
    total: u64,
    /// Part sizes for the install (connect, download, verify, open), summing to 1.
    parts: [f64; 4],
    shown: f64,
    /// When each step began (for the local timing record, `row`).
    marks: [Option<Instant>; 5],
    /// Each request's time, for the record.
    requests: Vec<f64>,
}

impl State {
    fn enter(&mut self, step: Step) {
        let now = Instant::now();
        self.step = step;
        self.since = now;
        self.marks[step as usize] = Some(now);
    }
}

pub struct Progress {
    state: Mutex<State>,
    timings: Timings,
}

impl Progress {
    fn new(step: Step, total: u64, planned: u32, timings: Timings) -> Arc<Self> {
        // Each part is as wide as its expected time, so the bar moves at an even pace. The
        // download (the one part counted end to end, and the longest on most connections) gets
        // at least 80 %: the timing file's rate comes from few, fast connections, and the
        // estimated parts must not outweigh the measured one (seen: half the bar on a default).
        let download = total as f64 * 8.0 / 1e6 / timings.mbps.max(0.1);
        let others = timings.connect + timings.verify + timings.open;
        let download = download.max(others * 4.0);
        let sum = (others + download).max(1e-6);
        let parts = [timings.connect / sum, download / sum, timings.verify / sum, timings.open / sum];
        let now = Instant::now();
        let mut marks = [None; 5];
        marks[step as usize] = Some(now);
        let state = State { step, since: now, answered: 0, planned, bytes: 0, total, parts, shown: 0.0, marks, requests: Vec::new() };
        Arc::new(Self { state: Mutex::new(state), timings })
    }

    /// "Check now": the usual check asks GitHub 3 times (release, tag on main, checksums).
    pub fn check(timings: &Timings) -> Arc<Self> {
        Self::new(Step::Checking, 0, 3, timings.clone())
    }

    /// The Setup download and hand-off for `total` bytes.
    pub fn install(total: u64, timings: &Timings) -> Arc<Self> {
        Self::new(Step::Connecting, total.max(1), 0, timings.clone())
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// The check found it needs more requests (a same-version hotfix: 6).
    pub fn plan(&self, requests: u32) {
        self.with(|s| s.planned = s.planned.max(requests));
    }

    /// One GitHub request answered (or given up on).
    pub fn answered(&self) {
        self.with(|s| {
            s.answered += 1;
            let now = Instant::now();
            s.requests.push(now.saturating_duration_since(s.since).as_secs_f64());
            s.since = now;
        });
    }

    /// Move on to `step` (ignored when it is not later).
    pub fn reach(&self, step: Step) {
        self.with(|s| {
            if step > s.step {
                s.enter(step);
            }
        });
    }

    /// `n` more Setup bytes saved.
    pub fn bytes(&self, n: u64) {
        self.with(|s| {
            if s.step < Step::Downloading {
                s.enter(Step::Downloading);
            }
            s.bytes = (s.bytes + n).min(s.total);
        });
    }

    pub fn view(&self, now: Instant) -> View {
        let t = &self.timings;
        self.with(|s| {
            let elapsed = now.saturating_duration_since(s.since);
            let [connect, download, verify, _] = s.parts;
            let (raw, label) = match s.step {
                Step::Checking => {
                    let planned = s.planned.max(1) as f64;
                    let raw = (s.answered as f64 + estimate(elapsed, t.request)) / planned;
                    (raw, format!("Asking GitHub · {} of {} answers", s.answered.min(s.planned), s.planned))
                }
                Step::Connecting => (connect * estimate(elapsed, t.connect), "Connecting to GitHub…".to_owned()),
                Step::Downloading => {
                    let mb = |b: u64| b as f64 / 1_048_576.0;
                    let raw = connect + download * (s.bytes as f64 / s.total as f64);
                    (raw, format!("Downloading Recall Setup · {:.1} of {:.1} MB", mb(s.bytes), mb(s.total)))
                }
                Step::Verifying => (connect + download + verify * estimate(elapsed, t.verify), "Checking the download…".to_owned()),
                Step::Opening => (connect + download + verify + s.parts[3] * estimate(elapsed, t.open), "Opening Setup…".to_owned()),
            };
            s.shown = s.shown.max(raw.clamp(0.0, NEVER_FULL));
            View { step: s.step, fraction: s.shown as f32, label }
        })
    }
}

impl Progress {
    /// Durations only (no names, paths or addresses) for `scripts/update-timings.py --app-log`:
    /// the check's request times, or the hand-off's connect, rate, checksum and open times.
    pub fn row(&self, done: Instant) -> serde_json::Value {
        self.with(|s| {
            let at = |step: Step| s.marks[step as usize];
            let span = |a: Step, b: Option<Instant>| at(a).zip(b).map(|(a, b)| b.saturating_duration_since(a).as_secs_f64());
            let r = |v: f64| (v * 1000.0).round() / 1000.0;
            let mut row = serde_json::Map::new();
            if !s.requests.is_empty() {
                row.insert("check_request_s".into(), s.requests.iter().map(|v| r(*v)).collect::<Vec<_>>().into());
            }
            if let Some(v) = span(Step::Connecting, at(Step::Downloading)) {
                row.insert("connect_s".into(), r(v).into());
            }
            if let Some(v) = span(Step::Downloading, at(Step::Verifying)).filter(|v| *v > 0.0 && s.bytes > 0) {
                row.insert("download_mbps".into(), r(s.bytes as f64 * 8.0 / 1e6 / v).into());
            }
            if let Some(v) = span(Step::Verifying, at(Step::Opening)) {
                row.insert("verify_s".into(), r(v).into());
            }
            if let Some(v) = span(Step::Opening, Some(done)).filter(|_| s.step == Step::Opening) {
                row.insert("open_setup_s".into(), r(v).into());
            }
            row.into()
        })
    }

    /// Append `row` to the local timing log (kept to its last 200 lines). Best effort.
    pub fn record(&self, file: &std::path::Path) {
        let row = self.row(Instant::now());
        if row.as_object().is_none_or(|o| o.is_empty()) {
            return;
        }
        let old = std::fs::read_to_string(file).unwrap_or_default();
        let mut lines: Vec<&str> = old.lines().collect();
        let skip = lines.len().saturating_sub(199);
        lines.drain(..skip);
        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&row.to_string());
        text.push('\n');
        let _ = file.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(file, text);
    }
}

/// The line under the bar: Setup's own part, from past updates (honest about the count).
pub fn setup_estimate(t: &Timings) -> String {
    let (median, p90, n) = t.setup;
    if n == 0 {
        return "Then Setup updates Recall and reopens it.".into();
    }
    let up = |s: f64| s.ceil().max(1.0) as u64;
    format!("Then Setup updates Recall and reopens it, usually in {}–{} s.", up(median), up(p90.max(median)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Timings {
        Timings { request: 0.5, connect: 1.0, mbps: 40.0, verify: 0.2, open: 0.6, setup: (7.0, 9.4, 4) }
    }

    #[test]
    fn the_shipped_timings_file_parses_with_every_phase() {
        let json = include_str!("update_timings.json");
        let doc: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
        for phase in ["check_request_s", "connect_s", "download_mbps", "verify_s", "open_setup_s", "setup_update_s"] {
            assert!(doc["phases"][phase]["median"].as_f64().is_some_and(|m| m > 0.0), "{phase} has a median");
        }
        assert_ne!(timings().request, 0.0);
        assert!(!json.to_ascii_lowercase().contains("users\\") && !json.contains(":\\"), "durations only, no paths");
    }

    #[test]
    fn a_missing_or_broken_file_falls_back_to_defaults() {
        assert_eq!(Timings::parse("not json").connect, 1.0);
        assert_eq!(Timings::parse(r#"{"phases":{"connect_s":{"median":-3}}}"#).connect, 1.0);
        assert_eq!(Timings::parse(r#"{"phases":{"connect_s":{"median":2.5}}}"#).connect, 2.5);
    }

    #[test]
    fn an_estimate_never_finishes_its_part() {
        assert!(estimate(Duration::ZERO, 1.0) == 0.0);
        assert!((estimate(Duration::from_secs(1), 1.0) - 0.45).abs() < 1e-9, "45 % at the median");
        assert!(estimate(Duration::from_secs(3600), 1.0) <= 0.9);
    }

    #[test]
    fn the_measured_download_is_at_least_80_percent_of_the_bar() {
        let fast = Timings { mbps: 10_000.0, ..t() };
        let [_, d, ..] = Progress::install(15_000_000, &fast).state.lock().unwrap().parts;
        assert!(d >= 0.8 - 1e-9, "{d}");
        let slow = Timings { mbps: 2.0, ..t() };
        let [_, d, ..] = Progress::install(15_000_000, &slow).state.lock().unwrap().parts;
        assert!(d > 0.95, "a slow download takes most of the bar: {d}");
    }

    #[test]
    fn the_download_part_follows_real_bytes() {
        let p = Progress::install(10_000_000, &t());
        let at = Instant::now();
        let start = p.view(at).fraction;
        p.bytes(5_000_000);
        let half = p.view(at);
        assert_eq!(half.step, Step::Downloading);
        let [c, d, ..] = p.state.lock().unwrap().parts;
        assert!((half.fraction as f64 - (c + d * 0.5)).abs() < 1e-6, "connect part + half the download part");
        assert!(half.fraction > start);
        assert!(half.label.contains("4.8 of 9.5 MB"), "{}", half.label);
    }

    #[test]
    fn the_bar_never_moves_back_or_fills_before_the_end() {
        let p = Progress::check(&t());
        let at = Instant::now();
        let mut last = 0.0;
        fn check(p: &Progress, at: Instant, last: &mut f32) {
            let f = p.view(at).fraction;
            assert!(f >= *last && f <= NEVER_FULL as f32, "{f} after {last}");
            *last = f;
        }
        check(&p, at + Duration::from_secs(2), &mut last);
        p.answered();
        check(&p, at + Duration::from_secs(2), &mut last);
        // A hotfix doubles the plan: the computed share drops, the shown bar holds.
        p.plan(6);
        check(&p, at + Duration::from_secs(2), &mut last);
        for _ in 0..10 {
            p.answered();
            check(&p, at + Duration::from_secs(60), &mut last);
        }

        let p = Progress::install(1000, &t());
        let mut last = 0.0;
        for step in [Step::Opening, Step::Verifying, Step::Connecting] {
            p.reach(step);
            check(&p, at + Duration::from_secs(600), &mut last);
        }
        p.bytes(10);
        assert_eq!(p.view(at).step, Step::Opening, "bytes after Opening do not pull the step back");
    }

    #[test]
    fn the_timing_record_holds_durations_only_and_stays_short() {
        let p = Progress::install(2_000_000, &t());
        let pause = || std::thread::sleep(Duration::from_millis(3));
        pause();
        p.bytes(2_000_000);
        pause();
        p.reach(Step::Verifying);
        pause();
        p.reach(Step::Opening);
        let row = p.row(Instant::now() + Duration::from_millis(500));
        let keys: Vec<_> = row.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["connect_s", "download_mbps", "verify_s", "open_setup_s"]);
        assert!(row["open_setup_s"].as_f64().unwrap() >= 0.5);

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("timings.jsonl");
        for _ in 0..205 {
            p.record(&file);
        }
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text.lines().count(), 200);
        assert!(text.lines().all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()));

        let check = Progress::check(&t());
        check.answered();
        assert!(check.row(Instant::now())["check_request_s"].as_array().is_some_and(|a| a.len() == 1));
    }

    #[test]
    fn the_check_counts_answers() {
        let p = Progress::check(&t());
        p.answered();
        let v = p.view(Instant::now());
        assert!(v.label.contains("1 of 3"), "{}", v.label);
        assert!(v.fraction >= 1.0 / 3.0 - 1e-6);
    }

    #[test]
    fn the_setup_line_quotes_past_updates_or_says_nothing_numeric() {
        assert!(setup_estimate(&t()).contains("7–10 s"), "{}", setup_estimate(&t()));
        let none = Timings { setup: (8.0, 12.0, 0), ..t() };
        assert!(!setup_estimate(&none).chars().any(|c| c.is_ascii_digit()));
    }
}
