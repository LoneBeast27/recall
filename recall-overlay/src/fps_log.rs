//! A rolling FPS record, so a drop can be traced afterwards. The stats poller already reads FPS
//! and 1% low once a second (from Windows' frame-timing events); each reading is kept here for
//! [`KEEP`] (in memory only). The record is written to a small CSV only when a hitch is seen
//! (FPS under [`HITCH_SHARE`] of the recent median), at most once per [`DUMP_GAP`], on a
//! background thread. No poller of its own: off whenever the stats are off. A new game starts
//! a new record (another game's FPS would read as a hitch).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Five minutes of one-per-second readings.
pub const KEEP: usize = 300;
/// A reading under this share of the median of the last [`BASELINE`] readings is a hitch.
pub const HITCH_SHARE: f32 = 0.6;
const BASELINE: usize = 30;
const DUMP_GAP: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq)]
struct Reading {
    unix_ms: u128,
    fps: Option<u32>,
    low1: Option<u32>,
    /// The pill was on screen (to tell its cost from the game's own dips).
    shown: bool,
}

#[derive(Debug, Default)]
pub struct FpsLog {
    ring: VecDeque<Reading>,
    last_dump: Option<Instant>,
    /// The game the readings are for.
    game: Option<u32>,
}

/// `%APPDATA%\recall-hdr\recall-overlay-fps.csv`, beside the overlay's log.
pub fn default_path() -> Option<PathBuf> {
    crate::user_dirs::appdata().map(|d| crate::user_dirs::checked(d.join("recall-hdr").join("recall-overlay-fps.csv")))
}

fn median(mut v: Vec<u32>) -> Option<u32> {
    v.sort_unstable();
    v.get(v.len() / 2).copied()
}

impl FpsLog {
    /// Keeps one reading; `Some(reason)` when it is a hitch worth writing out.
    pub fn push(&mut self, game: Option<u32>, fps: Option<u32>, low1: Option<u32>, shown: bool) -> Option<String> {
        if game != self.game {
            self.ring.clear();
            self.game = game;
        }
        let unix_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let recent: Vec<u32> = self.ring.iter().rev().take(BASELINE).filter_map(|r| r.fps).collect();
        if self.ring.len() == KEEP {
            self.ring.pop_front();
        }
        self.ring.push_back(Reading { unix_ms, fps, low1, shown });
        let base = (recent.len() >= 10).then(|| median(recent)).flatten()?;
        let now_fps = fps?;
        ((now_fps as f32) < base as f32 * HITCH_SHARE).then(|| format!("{now_fps} fps against a recent median of {base}"))
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// The record as CSV: `unix_ms,fps,low1,pill_shown`.
    pub fn csv(&self) -> String {
        let cell = |v: Option<u32>| v.map_or(String::new(), |n| n.to_string());
        let mut out = String::from("unix_ms,fps,low1,pill_shown\n");
        for r in &self.ring {
            out.push_str(&format!("{},{},{},{}\n", r.unix_ms, cell(r.fps), cell(r.low1), r.shown as u8));
        }
        out
    }

    /// The record to write, unless one was written less than [`DUMP_GAP`] ago.
    pub fn take_dump(&mut self, now: Instant) -> Option<String> {
        if self.last_dump.is_some_and(|t| now.saturating_duration_since(t) < DUMP_GAP) {
            return None;
        }
        self.last_dump = Some(now);
        Some(self.csv())
    }

    /// Writes `csv` to `path` on a background thread (never on the island's UI thread).
    pub fn write_in_background(path: &Path, csv: String) -> std::thread::JoinHandle<Result<(), String>> {
        let path = path.to_path_buf();
        std::thread::spawn(move || std::fs::write(&path, csv).map_err(|e| e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_under_sixty_percent_of_the_recent_median_is_a_hitch() {
        let mut log = FpsLog::default();
        for _ in 0..20 {
            assert_eq!(log.push(Some(1), Some(1000), Some(800), false), None);
        }
        assert_eq!(log.push(Some(1), Some(700), Some(500), true), None, "70%: a dip, not a hitch");
        assert!(log.push(Some(1), Some(500), Some(300), true).is_some_and(|r| r.contains("500 fps")));
        assert_eq!(log.push(Some(1), None, None, false), None, "no reading: nothing to judge");
    }

    #[test]
    fn it_keeps_five_minutes_and_writes_a_small_csv_at_most_once_a_minute() {
        let t0 = Instant::now();
        let mut log = FpsLog::default();
        for i in 0..400u32 {
            log.push(Some(1), Some(900 + i % 7), None, i % 2 == 0);
        }
        assert_eq!(log.len(), KEEP);
        let csv = log.csv();
        assert!(csv.starts_with("unix_ms,fps,low1,pill_shown\n") && csv.lines().count() == KEEP + 1);
        assert!(csv.len() < 16 * 1024, "tiny: {} bytes", csv.len());
        let dir = std::env::temp_dir().join(format!("recall-fps-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fps.csv");
        let csv = log.take_dump(t0).expect("a first write");
        FpsLog::write_in_background(&path, csv).join().unwrap().unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("unix_ms,fps"));
        assert_eq!(log.take_dump(t0 + Duration::from_secs(30)), None, "once a minute at most");
        assert!(log.take_dump(t0 + Duration::from_secs(61)).is_some());
        assert_eq!(log.push(Some(2), Some(100), None, false), None, "another game: a new record, no false hitch");
        assert_eq!(log.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
