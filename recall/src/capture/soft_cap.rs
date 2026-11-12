//! The capture-rate cap Recall keeps itself where Windows has none (before 11 24H2): frames
//! are kept on a schedule of their own timestamps (WGC `SystemRelativeTime`, 100 ns units),
//! so a burst after a callback hitch keeps the frames whose real times fall due, and the
//! average rate holds at the cap (a 144 Hz game capped at 66 fps keeps 66, not every third
//! frame).

/// Keeps one frame per interval of frame time.
#[derive(Debug)]
pub(crate) struct SoftCap {
    interval: i64,
    next_due: Option<i64>,
}

impl SoftCap {
    pub(crate) fn new(interval: std::time::Duration) -> Self {
        Self { interval: i64::try_from(interval.as_nanos() / 100).unwrap_or(i64::MAX).max(1), next_due: None }
    }

    /// Whether the frame with this timestamp is kept.
    pub(crate) fn keep(&mut self, timestamp: i64) -> bool {
        match self.next_due {
            Some(due) if timestamp < due => false,
            due => {
                // Stay on the schedule unless more than one interval behind (an idle game,
                // a pause), then start it again from this frame so no burst follows.
                let next = due.map(|d| d + self.interval).filter(|n| *n > timestamp).unwrap_or(timestamp + self.interval);
                self.next_due = Some(next);
                true
            }
        }
    }
}
