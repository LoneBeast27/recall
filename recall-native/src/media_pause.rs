//! Where a pause rests: on the frame the picture shows, not on the sound clock.
//!
//! Why: with sound, the clock is the sound's and the picture follows it loosely (the drift check
//! only pulls it back past 75 ms). A pause used to move the picture to the sound clock. The
//! picture usually runs a little ahead of the sound, so the seek showed a frame or two earlier:
//! the pause looked like a small jump back, and play went on from that earlier frame
//! (reported 2026-11-27). Now the sound moves to the picture's time and the frame stays put.

/// How far the picture may be from the sound clock and still be taken as the frame to rest on.
/// Wider than the drift check's 75 ms, which keeps a playing picture inside it; farther than
/// this the picture is not following the sound (stalled, still loading, unprimed and reading 0).
pub(super) const PICTURE_NEAR: f64 = 0.12;

/// How far the paused picture may move between the pause and the next play and still count as
/// where the pause left it: Media Foundation finishes a Pause a few milliseconds after it is
/// asked for, while its clock still runs.
pub(super) const HELD_SLACK: f64 = 0.05;

/// What a pause lines up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Rest {
    /// The picture stays: the sound clock moves to this picture time.
    Picture(f64),
    /// The picture moves to the sound clock at this time (the old way, for the cases above).
    Sound(f64),
}

/// `sound`: the sound clock at the pause. `picture`: the video player's time, when it was
/// playing and could be read. `picture_end`: where the picture's last frame starts.
pub(super) fn pause_rest(sound: f64, picture: Option<f64>, picture_end: Option<f64>) -> Rest {
    let past_end = |time: f64| picture_end.is_some_and(|end| time >= end);
    match picture {
        Some(at) if at.is_finite() && at >= 0.0 && (at - sound).abs() <= PICTURE_NEAR && !past_end(at) && !past_end(sound) => {
            Rest::Picture(at)
        }
        _ => Rest::Sound(sound),
    }
}

/// The paused picture still reads where the pause left it (`held`), within [`HELD_SLACK`]: play
/// starts the sound from the frame on screen.
pub(super) fn still_held(held: Option<f64>, at: Option<f64>) -> Option<f64> {
    let (held, at) = (held?, at?);
    ((at - held).abs() <= HELD_SLACK).then_some(at)
}

#[cfg(test)]
mod tests {
    use super::{Rest, pause_rest, still_held};

    #[test]
    fn a_pause_rests_on_the_frame_shown_not_the_sound_clock() {
        // 60 fps: the picture ran 30 ms (about two frames) ahead of the sound clock.
        assert_eq!(pause_rest(12.000, Some(12.030), Some(59.98)), Rest::Picture(12.030));
        // Behind the sound works the same way.
        assert_eq!(pause_rest(12.000, Some(11.970), Some(59.98)), Rest::Picture(11.970));
        assert_eq!(pause_rest(12.0, Some(12.0), None), Rest::Picture(12.0), "no known end");
    }

    #[test]
    fn a_picture_that_is_not_following_the_sound_moves_to_it() {
        assert_eq!(pause_rest(12.0, None, Some(59.98)), Rest::Sound(12.0), "picture not playing or unreadable");
        assert_eq!(pause_rest(12.0, Some(0.0), Some(59.98)), Rest::Sound(12.0), "unprimed player reads 0");
        assert_eq!(pause_rest(12.0, Some(11.5), Some(59.98)), Rest::Sound(12.0), "stalled half a second behind");
        assert_eq!(pause_rest(12.0, Some(f64::NAN), Some(59.98)), Rest::Sound(12.0));
    }

    #[test]
    fn past_the_last_frame_the_sound_keeps_its_clock() {
        // The sound plays on past the picture's last frame: the picture rests there on purpose.
        assert_eq!(pause_rest(60.5, Some(59.97), Some(59.98)), Rest::Sound(60.5));
        assert_eq!(pause_rest(59.95, Some(59.99), Some(59.98)), Rest::Sound(59.95));
    }

    #[test]
    fn play_starts_from_the_paused_frame_only_while_it_is_still_there() {
        assert_eq!(still_held(Some(12.030), Some(12.033)), Some(12.033), "the pause landed 3 ms later");
        assert_eq!(still_held(Some(12.030), Some(12.030)), Some(12.030));
        assert_eq!(still_held(Some(12.030), Some(14.0)), None, "the picture moved since");
        assert_eq!(still_held(None, Some(12.030)), None, "no pause rested on the picture");
        assert_eq!(still_held(Some(12.030), None), None, "position unreadable");
    }
}
