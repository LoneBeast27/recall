//! One undecodable audio packet in a clip: the stems and the edited export carry on past it
//! (silence in its place), as the ffmpeg CLI does, instead of failing the whole clip.

use std::path::{Path, PathBuf};

use ffmpeg_next as ff;

use super::test_media::{synth, Clip, Spec};
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

#[test]
fn a_bad_audio_packet_leaves_the_stem_whole() {
    let Some((clip, bad)) = bad_clip("bad-packet-stem") else { return };
    let cache = clip.dir.join("cache");
    let info = crate::editor_media::prepare_clip(&bad.to_string_lossy(), &cache.to_string_lossy())
        .expect("one bad packet must not stop the stems");
    let (samples, channels) = crate::editor_media::read_stem(Path::new(&info.audio[0].preview_path)).unwrap();
    let seconds = samples.len() as f64 / f64::from(channels) / 48_000.0;
    assert!((seconds - info.duration).abs() < 0.05, "stem {seconds} s for a {} s clip", info.duration);
}

#[test]
fn a_bad_audio_packet_does_not_stop_the_edited_audio() {
    let Some((_clip, bad)) = bad_clip("bad-packet-export") else { return };
    let plan = EditPlan { mix_down: Some(false), ranges: vec![TimeRange::new(0.0, 2.5)], audio: vec![] };
    let tracks = crate::edit_audio::encode_edited_audio(&bad, 0, &plan).expect("one bad packet must not stop the export");
    assert_eq!(tracks.len(), 1);
    assert!(!tracks[0].packets.is_empty());
}

/// Lag (samples) that best lines `bad` up with `good` around `at` seconds.
fn best_lag(good: &[f64], bad: &[f64], at: f64) -> i64 {
    let start = (at * 48_000.0) as i64;
    let sample = |s: &[f64], i: i64| s.get(i as usize).copied().unwrap_or(0.0);
    let cost = |lag: i64| (0..4800).map(|i| (sample(good, start + i) - sample(bad, start + i + lag)).powi(2)).sum::<f64>();
    (-80i64..=80).min_by(|a, b| cost(*a).total_cmp(&cost(*b))).unwrap()
}

#[test]
fn a_skipped_audio_packet_leaves_its_time_silent_and_the_rest_in_place() {
    // R2-4: the 21 ms of a skipped packet were closed up, so the rest of the track played
    // 1024 samples early in the preview and in the export.
    let Some((clip, bad)) = bad_clip("bad-packet-timing") else { return };
    let stem = |input: &Path, name: &str| {
        let info = crate::editor_media::prepare_clip(&input.to_string_lossy(), &clip.dir.join(name).to_string_lossy()).unwrap();
        crate::editor_media::read_stem(Path::new(&info.audio[0].preview_path)).unwrap()
    };
    let left = |(samples, channels): (Vec<i16>, u16)| samples.iter().step_by(usize::from(channels)).map(|&s| f64::from(s)).collect::<Vec<_>>();
    let good = left(stem(&clip.path, "cache-good"));
    let broken = left(stem(&bad, "cache-bad"));
    let lag = best_lag(&good, &broken, 2.0);
    assert!(lag.abs() <= 2, "after the bad packet the stem is {lag} samples off");
}

