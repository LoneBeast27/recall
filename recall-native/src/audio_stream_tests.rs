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
fn only_a_track_the_reader_has_passed_sends_it_back() {
    let second = SAMPLE_RATE as usize;
    assert_eq!(jump_to(None, 700, None), Some(700), "nothing read yet");
    assert_eq!(jump_to(Some(2.0), 0, None), None, "a late track waits for its first packet");
    assert_eq!(jump_to(Some(2.0), 0, Some(0)), Some(0), "a hole behind the reader");
    assert_eq!(jump_to(Some(2.0), 2 * second, Some(2 * second)), None, "at the reader");
    assert_eq!(jump_to(Some(1.0), 9 * second, None), Some(9 * second), "a seek ahead");
}

/// Mixes a clip with ffmpeg and decodes it the way a loading clip is. `track` commands are extra
/// inputs and options per sound track. Needs ffmpeg on PATH or RECALL_FFMPEG.
fn decoded(name: &str, build: Vec<Vec<&str>>, tracks: usize, seconds: f64) -> Arc<LiveSound> {
    use crate::services::{library::mp4_map, storage};
    use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
    // The sound streams from these files after this returns: earlier runs' folders go instead.
    crate::test_temp::sweep_dead("recall-live");
    let dir = std::env::temp_dir().join(format!("recall-live-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ffmpeg = storage::binary("RECALL_FFMPEG", "ffmpeg.exe").unwrap_or_else(|_| "ffmpeg.exe".into());
    for step in build {
        let status = std::process::Command::new(&ffmpeg).args(["-v", "error", "-y"]).args(&step).current_dir(&dir).status().expect("ffmpeg must be on PATH for this test");
        assert!(status.success(), "ffmpeg {step:?}");
    }
    let source = dir.join("clip.mp4");
    let metadata = source.metadata().unwrap();
    // Read where it lies: a clip this small would be copied and gone before the decoder opened it.
    let fill = ClipFill::direct(&source, metadata.len(), metadata.modified().unwrap(), 0);
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.unwrap();
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let map = Arc::new(mp4_map::clip_map(&mut std::fs::File::open(&source).unwrap()).unwrap());
    let media = crate::media::open_source(&fill, false).unwrap();
    let list: Vec<(usize, u16)> = (0..tracks).map(|index| (index + 1, 1)).collect();
    let sound = start(media, fill, map, &list, 0.0, seconds).unwrap();
    let started = std::time::Instant::now();
    // The decoder holds one handle until it has nothing left to do: a spin never lets go.
    while Arc::strong_count(&sound) > 1 {
        assert!(started.elapsed() < Duration::from_secs(20), "the decoder never finished; still missing {:?}", sound.tracks.iter().map(|t| t.1.missing()).collect::<Vec<_>>());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(sound.failed(), None);
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
    sound
}

/// The loudest sample of `[from, to)` seconds.
fn loudest(pcm: &LivePcm, from: f64, to: f64) -> f32 {
    let rate = SAMPLE_RATE as f64;
    ((from * rate) as usize..(to * rate) as usize).map(|at| pcm.sample(at * pcm.channels).abs()).fold(0.0, f32::max)
}

fn holes_in(pcm: &LivePcm, from: f64, to: f64) -> Vec<f64> {
    let mut at = from;
    let mut holes = Vec::new();
    while at < to {
        if loudest(pcm, at, at + 0.1) < 0.01 {
            holes.push((at * 10.0).round() / 10.0);
        }
        at += 0.1;
    }
    holes
}

fn picture() -> Vec<&'static str> {
    vec!["-f", "lavfi", "-i", "color=c=black:size=64x64:rate=30:duration=6", "-c:v", "libx264", "v.mp4"]
}

fn sound() -> Vec<&'static str> {
    vec!["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=6", "-c:a", "aac", "a.m4a"]
}

#[test]
fn a_clip_whose_only_track_starts_late_finishes_with_silence_before_it() {
    let live = decoded("late-one", vec![picture(), sound(), vec!["-i", "v.mp4", "-itsoffset", "3", "-i", "a.m4a", "-map", "0:v", "-map", "1:a", "-c", "copy", "clip.mp4"]], 1, 6.0);
    let pcm = &live.tracks[0].1;
    assert!(pcm.missing().is_empty());
    assert!(loudest(pcm, 0.0, 2.8) < 0.01, "silence before the track starts");
    assert_eq!(holes_in(pcm, 3.2, 5.8), Vec::<f64>::new());
}

#[test]
fn a_late_second_track_leaves_the_first_one_whole() {
    let live = decoded("late-two", vec![picture(), sound(), vec!["-i", "v.mp4", "-i", "a.m4a", "-itsoffset", "3", "-i", "a.m4a", "-map", "0:v", "-map", "1:a", "-map", "2:a", "-c", "copy", "clip.mp4"]], 2, 6.0);
    assert_eq!(holes_in(&live.tracks[0].1, 0.1, 5.8), Vec::<f64>::new(), "the healthy track has no silent parts");
    assert!(loudest(&live.tracks[1].1, 0.0, 2.8) < 0.01);
    assert_eq!(holes_in(&live.tracks[1].1, 3.2, 5.8), Vec::<f64>::new());
}

/// Three tracks of `lengths` seconds, so each one can be told by where it ends.
fn three_tracks(name: &str, lengths: [&str; 3]) -> Vec<f64> {
    let mut clip = vec!["-i", "v.mp4", "-i", "a.m4a"];
    for length in &lengths {
        clip.extend(["-t", length, "-i", "a.m4a"]);
    }
    clip.extend(["-map", "0:v", "-map", "2:a", "-map", "3:a", "-map", "4:a", "-c", "copy", "clip.mp4"]);
    let live = decoded(name, vec![picture(), sound(), clip], 3, 6.0);
    live.tracks.iter().map(|(_, pcm)| (0..60).map(|tenth| tenth as f64 / 10.0).filter(|at| loudest(pcm, *at, at + 0.1) >= 0.01).fold(0.0, f64::max).ceil()).collect()
}

#[test]
fn sound_streams_come_in_file_order() {
    assert_eq!(three_tracks("order-a", ["2", "4", "6"]), [2.0, 4.0, 6.0]);
    assert_eq!(three_tracks("order-b", ["6", "2", "4"]), [6.0, 2.0, 4.0]);
}

#[test]
fn a_gap_in_one_track_leaves_the_other_whole() {
    let live = decoded("gap", vec![picture(), sound(), vec!["-i", "v.mp4", "-i", "a.m4a", "-t", "4", "-i", "a.m4a", "-map", "0:v", "-map", "1:a", "-map", "2:a", "-c", "copy", "-bsf:a:1", "setts=ts=if(gte(TS\\,96000)\\,TS+48000\\,TS)", "clip.mp4"]], 2, 6.0);
    assert_eq!(holes_in(&live.tracks[0].1, 0.1, 5.8), Vec::<f64>::new(), "the healthy track has no silent parts");
    assert!(loudest(&live.tracks[1].1, 2.2, 2.8) < 0.01, "the real gap stays silent");
    assert_eq!(holes_in(&live.tracks[1].1, 3.2, 4.8), Vec::<f64>::new());
}

#[test]
fn packets_at_other_rates_come_out_at_48_khz() {
    let pcm: Vec<i16> = (0..441).map(|i| i as i16).collect();
    let out = resample(&pcm, 1, 44_100);
    assert_eq!(out.len(), 480);
    assert_eq!(out[0], 0);
    assert!((out[240] as i32 - 220).abs() <= 1);
}
