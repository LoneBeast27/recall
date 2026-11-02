//! Where pressing play starts and what playback does at the edges of the kept ranges. Pure
//! rules (unit tested); `EditorState` applies them to the native preview.
//!
//! The kept-range rules hold only while the playhead is in a kept range. Play pressed with the
//! playhead outside them (before In, in a cut, after Out) runs "free" through the cut footage,
//! with no snap to In; once it reaches a kept range the kept-range rules apply again.
//!
//! `limit` is the latest time the player's clock can reach (the clip's end, or the picture's
//! last frame for a clip without sound): a kept range never plays past it.
use crate::edit_model::TimeRange;

/// Less than this left to play counts as "at the end": play starts over (two frames at 60 fps).
pub const END_SLACK: f64 = 0.03;
/// The clock is this close to a range's end: the range has finished.
const AT_EDGE: f64 = 0.001;

/// What playback does with the playhead at a given time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Follow {
    /// Inside a kept range: keep playing.
    Keep,
    /// In a cut (or before the first kept range): continue at the next kept range's start.
    Jump(f64),
    /// Past the end of the last kept range (free playback: the clip's end): stop.
    Stop,
    /// Free playback reached a kept range: the kept-range rules apply from here.
    Enter,
}

/// True when play pressed with the playhead at `pos` runs free: it sits outside every kept
/// range (a range's end and a poll's overshoot past it count as inside) and the clip has
/// something left to play. A finished clip still starts over at the first kept range.
pub fn plays_free(ranges: &[TimeRange], pos: f64, limit: f64) -> bool {
    if ranges.is_empty() || pos >= limit - END_SLACK {
        return false;
    }
    !ranges.iter().any(|r| pos >= r.start - AT_EDGE && pos <= r.end.min(limit) + END_SLACK)
}

/// What free playback does next with the clock at `pos`: run on through cut footage, enter a
/// kept range when it reaches one, and stop at the clip's end.
pub fn follow_free(ranges: &[TimeRange], pos: f64, limit: f64) -> Follow {
    if ranges.iter().any(|r| pos >= r.start - AT_EDGE && pos < r.end.min(limit) - AT_EDGE) {
        return Follow::Enter;
    }
    if pos >= limit - AT_EDGE { Follow::Stop } else { Follow::Keep }
}

/// Where play starts for the playhead at `pos`: right there when it is inside a kept range
/// with something left to play; at the next kept range's start when it sits in a cut or at a
/// range's end; and at the start of the first kept range when it is at or past the end of the
/// last one (a finished clip plays again from the top).
pub fn play_from(ranges: &[TimeRange], pos: f64, limit: f64) -> f64 {
    let Some(first) = ranges.first() else { return pos.max(0.0) };
    for range in ranges {
        let end = range.end.min(limit);
        if end - range.start < END_SLACK {
            continue; // nothing playable left in it (clipped by the limit)
        }
        if pos < end - END_SLACK {
            return pos.max(range.start);
        }
    }
    first.start
}

/// What playback does next with the clock at `pos`.
pub fn follow(ranges: &[TimeRange], pos: f64, limit: f64) -> Follow {
    for range in ranges {
        if pos < range.start - AT_EDGE {
            return Follow::Jump(range.start);
        }
        if pos < range.end.min(limit) - AT_EDGE {
            return Follow::Keep;
        }
    }
    Follow::Stop
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(spans: &[(f64, f64)]) -> Vec<TimeRange> {
        spans.iter().map(|&(start, end)| TimeRange::new(start, end)).collect()
    }

    #[test]
    fn play_inside_a_range_starts_right_there() {
        let kept = ranges(&[(0.0, 10.453)]);
        assert_eq!(play_from(&kept, 4.2, 10.453), 4.2);
        assert_eq!(play_from(&kept, 0.0, 10.453), 0.0);
    }

    #[test]
    fn play_at_the_end_of_the_clip_starts_over() {
        let kept = ranges(&[(0.0, 10.453)]);
        // Finished on its own (the clock overshoots by a poll), pressed End, or a hair before it.
        for pos in [10.453, 10.47, 10.44, 10.425] {
            assert_eq!(play_from(&kept, pos, 10.453), 0.0, "{pos}");
        }
    }

    #[test]
    fn play_at_the_end_of_a_trimmed_segment_starts_at_its_start() {
        // User 2026-10-30 11:25: 0.91 s kept of a 3.8 s clip, playhead at 0.91.
        let kept = ranges(&[(0.0, 0.91)]);
        for pos in [0.91, 0.913, 0.9, 3.8] {
            assert_eq!(play_from(&kept, pos, 3.8), 0.0, "{pos}");
        }
        let late = ranges(&[(1.25, 2.0)]);
        assert_eq!(play_from(&late, 2.0, 3.8), 1.25);
        assert_eq!(play_from(&late, 0.4, 3.8), 1.25, "before the kept part");
    }

    #[test]
    fn play_in_a_cut_or_at_a_middle_segment_end_goes_to_the_next_segment() {
        let kept = ranges(&[(0.0, 2.0), (3.0, 5.0), (7.0, 9.0)]);
        assert_eq!(play_from(&kept, 2.5, 10.0), 3.0, "in the cut");
        assert_eq!(play_from(&kept, 2.0, 10.0), 3.0, "at the first segment's end");
        assert_eq!(play_from(&kept, 4.99, 10.0), 7.0, "at the middle segment's end");
        assert_eq!(play_from(&kept, 9.0, 10.0), 0.0, "at the last segment's end");
        assert_eq!(play_from(&kept, 9.5, 10.0), 0.0, "after the last segment");
    }

    #[test]
    fn play_respects_the_clock_limit() {
        // No sound: the picture's clock stops at the last frame (10.428), short of 10.453.
        let kept = ranges(&[(0.0, 10.453)]);
        assert_eq!(play_from(&kept, 10.428, 10.438), 0.0);
        assert_eq!(play_from(&kept, 5.0, 10.438), 5.0);
        // A segment wholly past the limit is skipped.
        let tail = ranges(&[(0.0, 2.0), (10.44, 10.453)]);
        assert_eq!(play_from(&tail, 2.0, 10.438), 0.0);
    }

    #[test]
    fn very_short_segments_still_play() {
        let kept = ranges(&[(1.0, 1.05)]);
        assert_eq!(play_from(&kept, 1.0, 3.8), 1.0);
        assert_eq!(play_from(&kept, 1.05, 3.8), 1.0);
    }

    #[test]
    fn follow_keeps_jumps_cuts_and_stops_at_the_end() {
        let kept = ranges(&[(0.0, 2.0), (3.0, 5.0)]);
        assert_eq!(follow(&kept, 1.0, 10.0), Follow::Keep);
        assert_eq!(follow(&kept, 2.0, 10.0), Follow::Jump(3.0));
        assert_eq!(follow(&kept, 2.6, 10.0), Follow::Jump(3.0));
        assert_eq!(follow(&kept, 4.5, 10.0), Follow::Keep);
        assert_eq!(follow(&kept, 4.9995, 10.0), Follow::Stop);
        assert_eq!(follow(&kept, 6.0, 10.0), Follow::Stop);
        let late = ranges(&[(1.25, 2.0)]);
        assert_eq!(follow(&late, 0.0, 10.0), Follow::Jump(1.25));
    }

    #[test]
    fn follow_stops_at_the_clock_limit() {
        let kept = ranges(&[(0.0, 10.453)]);
        assert_eq!(follow(&kept, 10.4375, 10.438), Follow::Stop);
        assert_eq!(follow(&kept, 10.4, 10.438), Follow::Keep);
    }

    #[test]
    fn play_inside_the_kept_range_keeps_its_rules() {
        // In 2.0, Out 5.0 of a 10 s clip.
        let kept = ranges(&[(2.0, 5.0)]);
        assert!(!plays_free(&kept, 3.0, 10.0));
        assert_eq!(play_from(&kept, 3.0, 10.0), 3.0);
        assert_eq!(follow(&kept, 4.9995, 10.0), Follow::Stop, "playback ends at Out");
        for pos in [5.0, 5.02, 4.99] {
            assert!(!plays_free(&kept, pos, 10.0), "{pos}: resting at Out is inside");
            assert_eq!(play_from(&kept, pos, 10.0), 2.0, "{pos}: play again goes back to In");
        }
    }

    #[test]
    fn play_before_in_runs_on_into_the_range_without_a_snap() {
        let kept = ranges(&[(2.0, 5.0)]);
        assert!(plays_free(&kept, 0.5, 10.0), "no jump to In");
        assert_eq!(follow_free(&kept, 0.5, 10.0), Follow::Keep);
        assert_eq!(follow_free(&kept, 1.99, 10.0), Follow::Keep);
        // Reaching In: the kept range takes over, and its rules end playback at Out.
        assert_eq!(follow_free(&kept, 2.0, 10.0), Follow::Enter);
        assert_eq!(follow(&kept, 4.9995, 10.0), Follow::Stop);
    }

    #[test]
    fn play_after_out_runs_to_the_clip_end_and_stops() {
        let kept = ranges(&[(2.0, 5.0)]);
        assert!(plays_free(&kept, 6.0, 10.0), "no jump to In");
        assert_eq!(follow_free(&kept, 6.0, 10.0), Follow::Keep);
        assert_eq!(follow_free(&kept, 9.9, 10.0), Follow::Keep);
        assert_eq!(follow_free(&kept, 10.0, 10.0), Follow::Stop);
        // Stopped at the clip's end: play again starts the kept part over.
        assert!(!plays_free(&kept, 10.0, 10.0));
        assert_eq!(play_from(&kept, 10.0, 10.0), 2.0);
    }

    #[test]
    fn playhead_moved_out_of_the_range_while_paused_plays_free() {
        let kept = ranges(&[(2.0, 5.0)]);
        assert!(!plays_free(&kept, 4.0, 10.0), "paused inside");
        // Dragged past Out while paused, then play: it runs on, no loop back to In.
        assert!(plays_free(&kept, 7.0, 10.0));
        assert_eq!(follow_free(&kept, 7.5, 10.0), Follow::Keep);
        // Dragged before In while paused, then play: same.
        assert!(plays_free(&kept, 1.0, 10.0));
    }

    #[test]
    fn play_in_a_cut_between_kept_ranges_runs_into_the_next_range() {
        let kept = ranges(&[(0.0, 2.0), (3.0, 5.0)]);
        assert!(plays_free(&kept, 2.5, 10.0));
        assert_eq!(follow_free(&kept, 2.9, 10.0), Follow::Keep);
        assert_eq!(follow_free(&kept, 3.0, 10.0), Follow::Enter);
        assert!(!plays_free(&kept, 2.0, 10.0), "at a range's end is inside");
    }

    #[test]
    fn an_untrimmed_clip_never_plays_free() {
        let whole = ranges(&[(0.0, 10.0)]);
        for pos in [0.0, 5.0, 9.99, 10.0] {
            assert!(!plays_free(&whole, pos, 10.0), "{pos}");
        }
    }

    #[test]
    fn free_play_respects_the_clock_limit() {
        // No sound: the picture's clock stops at 10.438, short of the 10.453 duration.
        let kept = ranges(&[(2.0, 5.0)]);
        assert!(!plays_free(&kept, 10.43, 10.438), "nothing left past Out: starts over");
        assert_eq!(follow_free(&kept, 10.4375, 10.438), Follow::Stop);
    }

    #[test]
    fn nothing_kept_plays_where_it_is() {
        assert_eq!(play_from(&[], 3.0, 10.0), 3.0);
        assert_eq!(follow(&[], 3.0, 10.0), Follow::Stop);
        assert!(!plays_free(&[], 3.0, 10.0));
    }
}
