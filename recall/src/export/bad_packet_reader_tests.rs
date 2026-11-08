//! One undecodable audio packet in the edited export (`track_reader`): the export carries on
//! past it, and its time stays silent instead of pulling the rest of the track early.
//! (The stem side of the same bug is covered in `bad_packet_tests`.)

use std::path::{Path, PathBuf};

use ffmpeg_next as ff;

use super::test_media::{synth, Clip, Spec};
use super::track_reader::TrackReader;
use crate::edit_model::{EditPlan, TimeRange};

/// `clip` copied to `bad.mp4` with the `nth` packet of its first audio stream overwritten.
fn with_bad_audio_packet(clip: &Clip, nth: usize) -> PathBuf {
    let out_path = clip.dir.join("bad.mp4");
    let mut input = ff::format::input(&clip.path).unwrap();
    let mut output = ff::format::output(&out_path).unwrap();
    let audio = input.streams().best(ff::media::Type::Audio).unwrap().index();
    let mut time_bases = Vec::new();
    for stream in input.streams() {
        let mut out = output.add_stream(ff::encoder::find(ff::codec::Id::None)).unwrap();
        out.set_parameters(stream.parameters());
        unsafe { (*out.parameters().as_mut_ptr()).codec_tag = 0 };
        time_bases.push(stream.time_base());
    }
    output.write_header().unwrap();
    let mut seen = 0usize;
    for (stream, mut packet) in input.packets() {
        if stream.index() == audio {
            if seen == nth {
                // A reserved syntax element up front: the AAC decoder rejects the packet.
                if let Some(data) = packet.data_mut() {
                    data.fill(0xff);
                }
            }
            seen += 1;
        }
        let index = stream.index();
        let out_tb = output.stream(index).unwrap().time_base();
        packet.rescale_ts(time_bases[index], out_tb);
        packet.set_position(-1);
        packet.set_stream(index);
        packet.write_interleaved(&mut output).unwrap();
    }
    output.write_trailer().unwrap();
    out_path
}

fn bad_clip(tag: &str) -> Option<(Clip, PathBuf)> {
    let Some(clip) = synth(tag, &Spec { tones: vec![440.0], seconds: 3.0, ..Spec::default() }) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return None;
    };
    let bad = with_bad_audio_packet(&clip, 40);
    Some((clip, bad))
}

/// Ported from fix/deep-editor (57eb2dd), where the export decoded through `decode_track`.
#[test]
fn a_bad_audio_packet_does_not_stop_the_edited_audio() {
    let Some((_clip, bad)) = bad_clip("bad-packet-export") else { return };
    let plan = EditPlan { mix_down: Some(false), ranges: vec![TimeRange::new(0.0, 2.5)], audio: vec![] };
    let tracks = crate::edit_audio::encode_edited_audio(&bad, 0, &plan).expect("one bad packet must not stop the export");
    assert_eq!(tracks.len(), 1);
    assert!(!tracks[0].packets.is_empty());
}

/// First-channel PCM of the edited audio of `input`'s first audio stream over `ranges`.
fn edited_left(input: &Path, ranges: &[TimeRange]) -> Vec<f64> {
    let index = crate::edit_audio::audio_indices(input).unwrap()[0];
    let mut reader = TrackReader::open(input, 0, ranges, index, None, None).unwrap();
    let mut left = Vec::new();
    while !reader.exhausted() {
        reader.fill(4096).unwrap();
        let count = reader.available();
        left.extend(reader.take(count)[0].iter().map(|&s| f64::from(s)));
    }
    left
}

/// Lag (samples) that best lines `bad` up with `good` around `at` seconds.
fn best_lag(good: &[f64], bad: &[f64], at: f64) -> i64 {
    let start = (at * 48_000.0) as i64;
    let sample = |s: &[f64], i: i64| s.get(i as usize).copied().unwrap_or(0.0);
    let cost = |lag: i64| (0..4800).map(|i| (sample(good, start + i) - sample(bad, start + i + lag)).powi(2)).sum::<f64>();
    (-80i64..=80).min_by(|a, b| cost(*a).total_cmp(&cost(*b))).unwrap()
}

#[test]
fn a_skipped_audio_packet_keeps_the_rest_of_the_edited_audio_in_place() {
    // R2-deep-editor-4, export side: the 21 ms of a skipped packet were closed up, so the rest
    // of the track came out 1024 samples early.
    let Some((clip, bad)) = bad_clip("bad-packet-reader-timing") else { return };
    let ranges = [TimeRange::new(0.0, 2.8)];
    let (good, broken) = (edited_left(&clip.path, &ranges), edited_left(&bad, &ranges));
    assert_eq!(good.len(), broken.len(), "same length either way");
    let lag = best_lag(&good, &broken, 2.0);
    assert!(lag.abs() <= 2, "after the bad packet the edited audio is {lag} samples off");
}
