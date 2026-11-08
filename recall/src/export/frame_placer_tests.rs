use super::*;
use crate::edit_model::TimeRange;
use crate::export::edit_pcm::range_sample_bounds;

const RATE: u32 = 48_000;
const FRAME: i64 = 1024;

/// Mono frames of 0.5 at the given starts, edited to `range` the way `TrackReader` does.
fn edit(starts: &[i64], range: TimeRange) -> Vec<f32> {
    let bounds = range_sample_bounds(&[range], RATE).unwrap();
    let mut window = SourceWindow::new(1, bounds[0].source_start, bounds[0].source_end);
    let mut editor = RangeEditor::new(bounds, RATE, ffmpeg_next::ChannelLayout::MONO);
    let mut out: OutQueue = vec![VecDeque::new()];
    let mut placer = Placer::new(i64::from(RATE));
    let (mut frames, mut expected) = (starts.iter(), None::<i64>);
    fill(&mut placer, &mut window, &mut editor, &mut out, 1.0, usize::MAX, |placer| {
        match frames.next() {
            Some(&start) => {
                placer.push(start, expected, vec![vec![0.5; FRAME as usize]]);
                expected = Some(start + FRAME);
            }
            None => placer.end_input(),
        }
        Ok(())
    })
    .unwrap();
    out[0].iter().copied().collect()
}

#[test]
fn a_lone_far_future_stamp_does_not_silence_the_rest() {
    // R2-1: frames at 0, 1024, then one stamped 10^9 samples ahead, then 2048, 3072, ... again.
    let mut starts = vec![0, FRAME, 1_000_000_000];
    starts.extend((2..60).map(|n| n * FRAME));
    let pcm = edit(&starts, TimeRange::new(0.0, 1.0));
    assert_eq!(pcm.len(), 48_000);
    let silent = pcm.iter().filter(|&&s| s == 0.0).count();
    assert_eq!(silent, 0, "{silent} of 48000 samples silent after the lone stamp");
}

#[test]
fn a_real_forward_gap_stays_silent_and_the_audio_after_it_stays_in_place() {
    // 10 frames, then the stream resumes 1.5 s in and carries on from there.
    let mut starts: Vec<i64> = (0..10).map(|n| n * FRAME).collect();
    starts.extend((0..40).map(|n| 72_000 + n * FRAME));
    let pcm = edit(&starts, TimeRange::new(0.0, 2.0));
    assert_eq!(pcm.len(), 96_000);
    assert!(pcm[..10 * FRAME as usize].iter().all(|&s| s == 0.5), "before the gap");
    assert!(pcm[10 * FRAME as usize..72_000].iter().all(|&s| s == 0.0), "the gap is silent");
    assert!(pcm[72_000..].iter().all(|&s| s == 0.5), "after the gap, at its own time");
}
