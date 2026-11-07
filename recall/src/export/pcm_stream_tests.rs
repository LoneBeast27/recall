use super::*;
use crate::edit_model::TimeRange;
use crate::export::edit_pcm::range_sample_bounds;

/// A ramp so every sample is distinct: sample n of channel c is n + c / 2.
fn source(len: usize) -> Vec<Vec<f32>> {
    (0..2).map(|c| (0..len).map(|n| n as f32 + c as f32 * 0.5).collect()).collect()
}

/// Feeds `source` in frames of `frame` samples, pumping with a small budget the way the
/// track reader does, and returns everything that came out.
fn stream(ranges: &[TimeRange], rate: u32, frame: usize, gain: f32) -> Vec<Vec<f32>> {
    let bounds = range_sample_bounds(ranges, rate).unwrap();
    let total = source(bounds.last().unwrap().source_end as usize + 5_000);
    let mut window = SourceWindow::new(2, bounds[0].source_start, bounds.last().unwrap().source_end);
    let mut editor = RangeEditor::new(bounds, rate, ff::ChannelLayout::STEREO);
    let mut out: OutQueue = vec![VecDeque::new(); 2];
    let mut drained = vec![Vec::new(); 2];
    let margin = rate as i64;
    let mut start = 0usize;
    while start < total[0].len() {
        let end = (start + frame).min(total[0].len());
        while editor.pump(&mut window, start as i64 - margin, 300, &mut out).unwrap() {
            for (all, part) in drained.iter_mut().zip(take(&mut out, usize::MAX)) {
                all.extend(part);
            }
        }
        let planes: Vec<Vec<f32>> = total.iter().map(|c| c[start..end].to_vec()).collect();
        window.write(start as i64, &planes, gain);
        start = end;
    }
    while editor.pump(&mut window, i64::MAX, 300, &mut out).unwrap() {}
    assert!(editor.done());
    for (all, part) in drained.iter_mut().zip(take(&mut out, usize::MAX)) {
        all.extend(part);
    }
    drained
}

#[test]
fn streamed_ranges_match_the_source_sample_for_sample() {
    let rate = 1_000;
    let ranges = [TimeRange::new(0.5, 2.25), TimeRange::new(3.0, 3.001), TimeRange::new(7.5, 9.0)];
    let got = stream(&ranges, rate, 1_024, 0.5);
    let full = source(10_000);
    let expected: Vec<Vec<f32>> = (0..2)
        .map(|c| [(500, 2_250), (3_000, 3_001), (7_500, 9_000)].iter().flat_map(|&(a, b)| full[c][a..b].iter().map(|s| s * 0.5)).collect())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn the_window_stays_short_however_long_the_range() {
    let bounds = range_sample_bounds(&[TimeRange::new(0.0, 600.0)], 1_000).unwrap();
    let mut window = SourceWindow::new(1, 0, bounds[0].source_end);
    let mut editor = RangeEditor::new(bounds, 1_000, ff::ChannelLayout::MONO);
    let mut out: OutQueue = vec![VecDeque::new()];
    let mut emitted = 0usize;
    for start in (0..600_000i64).step_by(1_000) {
        while editor.pump(&mut window, start - 1_000, usize::MAX, &mut out).unwrap() {}
        emitted += take(&mut out, usize::MAX)[0].len();
        window.write(start, &[vec![1.0; 1_000]], 1.0);
        assert!(window.data[0].len() <= 2_000, "window grew to {} samples", window.data[0].len());
    }
    while editor.pump(&mut window, i64::MAX, usize::MAX, &mut out).unwrap() {}
    emitted += take(&mut out, usize::MAX)[0].len();
    assert_eq!(emitted, 600_000);
}

#[test]
fn a_slowed_range_comes_out_at_exactly_its_stretched_length() {
    ff::init().unwrap();
    let rate = 48_000;
    let ranges = [TimeRange { start: 0.0, end: 1.0, speed: 0.5 }, TimeRange { start: 2.0, end: 3.0, speed: 2.0 }];
    let got = stream(&ranges, rate, 1_024, 1.0);
    assert_eq!(got[0].len(), 96_000 + 24_000);
    assert_eq!(got[1].len(), got[0].len());
}

#[test]
fn missing_source_reads_as_silence() {
    let bounds = range_sample_bounds(&[TimeRange::new(0.0, 1.0)], 100).unwrap();
    let mut window = SourceWindow::new(1, 0, 100);
    let mut editor = RangeEditor::new(bounds, 100, ff::ChannelLayout::MONO);
    let mut out: OutQueue = vec![VecDeque::new()];
    window.write(40, &[vec![1.0; 10]], 1.0);
    while editor.pump(&mut window, i64::MAX, usize::MAX, &mut out).unwrap() {}
    let got = take(&mut out, usize::MAX).remove(0);
    assert_eq!(got.len(), 100);
    assert_eq!(got.iter().filter(|s| **s == 1.0).count(), 10);
    assert!(got[..40].iter().chain(&got[50..]).all(|s| *s == 0.0));
}
