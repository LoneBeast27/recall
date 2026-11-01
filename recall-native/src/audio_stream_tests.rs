use super::*;

#[test]
fn decoded_runs_merge_and_the_next_gap_follows_the_playhead() {
    let mut runs = Vec::new();
    insert_run(&mut runs, 100, 200);
    insert_run(&mut runs, 300, 400);
    assert_eq!(gap_after(&runs, 0, 1000), Some(0));
    assert_eq!(gap_after(&runs, 150, 1000), Some(200));
    assert_eq!(gap_after(&runs, 350, 1000), Some(400));
    insert_run(&mut runs, 190, 310);
    assert_eq!(runs, vec![(100, 400)]);
    insert_run(&mut runs, 400, 1000);
    assert_eq!(gap_after(&runs, 500, 1000), Some(0), "wraps to what the seek skipped");
    insert_run(&mut runs, 0, 100);
    assert_eq!(gap_after(&runs, 500, 1000), None);
}

#[test]
fn frames_read_as_silence_until_decoded_and_peaks_fill_in() {
    let pcm = LivePcm::new(WAVE_BINS * 2, 2);
    assert_eq!(pcm.sample(0), 0.0);
    assert!(!pcm.has(0, 4));
    pcm.write(0, &[16384, -16384, 8192, 8192]);
    assert_eq!(pcm.sample(0), 0.5);
    assert_eq!(pcm.sample(1), -0.5);
    assert!(pcm.has(0, 2));
    assert!(!pcm.has(0, 3));
    assert_eq!(pcm.peaks()[0], 0.5);
    assert_eq!(pcm.peaks()[1], 0.0);
}

#[test]
fn writes_past_the_clip_are_clipped() {
    let pcm = LivePcm::new(2, 1);
    pcm.write(1, &[1, 2, 3]);
    assert!(pcm.has(1, 2));
    assert_eq!(pcm.next_gap(0), Some(0));
}

#[test]
fn a_track_that_starts_late_has_silence_before_it_not_a_gap() {
    assert_eq!(packet_start(None, Some(0), 144_000), (144_000, Some((0, 144_000))), "starts 3 s into the clip");
    assert_eq!(packet_start(None, Some(48_000), 47_500), (47_500, None), "a packet from just before the seek");
    assert_eq!(packet_start(Some(1000), None, 1500), (1000, None), "jitter stays back to back");
    assert_eq!(packet_start(Some(1000), None, 9000), (9000, Some((1000, 9000))), "a real gap");
    assert_eq!(packet_start(None, None, 700), (700, None));
}

#[test]
fn a_stamp_past_the_clip_is_dropped_not_a_crash() {
    let pcm = LivePcm::new(10, 2);
    pcm.write(50, &[1, 2]);
    pcm.write(10, &[1, 2]);
    assert!(!pcm.has(0, 1));
}

#[test]
fn packets_at_other_rates_come_out_at_48_khz() {
    let pcm: Vec<i16> = (0..441).map(|i| i as i16).collect();
    let out = resample(&pcm, 1, 44_100);
    assert_eq!(out.len(), 480);
    assert_eq!(out[0], 0);
    assert!((out[240] as i32 - 220).abs() <= 1);
}
