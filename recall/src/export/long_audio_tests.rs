//! Edited audio longer than the old 256 MiB-per-track PCM buffer (about 11.6 min of 48 kHz
//! stereo): the audio is decoded, stretched, mixed and encoded in chunks, so it has no limit.

use ffmpeg_next as ff;

use super::test_media::{synth, Spec, RATE};
use super::timeline::timeline_origin_us;
use crate::edit_model::{AudioSetting, EditPlan, TimeRange};

/// Output seconds of one encoded track (AAC frames of 1024 samples).
fn seconds(track: &crate::edit_audio::EncodedAudioTrack) -> f64 {
    track.packets.len() as f64 * 1024.0 / track.rate as f64
}

#[test]
fn twelve_minutes_of_slow_motion_audio_export_mixed_and_separate() {
    // 3 min of source at 1 fps (the video is irrelevant here), slowed to 0.25x = 12 min out.
    let spec = Spec { width: 64, height: 64, fps: 1, gop: 1, seconds: 180.0, ..Spec::default() };
    let Some(clip) = synth("long-audio", &spec) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let origin = {
        let input = ff::format::input(&clip.path).unwrap();
        let video = input.streams().best(ff::media::Type::Video).unwrap().index();
        timeline_origin_us(&input, video)
    };
    let ranges = vec![TimeRange { start: 0.0, end: 180.0, speed: 0.25 }];
    let mixed = |mix| vec![
        AudioSetting { stream_index: 1, gain_db: 0.0, muted: false, mix },
        AudioSetting { stream_index: 2, gain_db: 0.0, muted: false, mix },
    ];
    let expected = 720.0;
    for (mix_down, tracks) in [(true, 1usize), (false, 2)] {
        let plan = EditPlan { mix_down: Some(mix_down), ranges: ranges.clone(), audio: mixed(mix_down) };
        let encoded = crate::edit_audio::encode_edited_audio(&clip.path, origin, &plan)
            .unwrap_or_else(|error| panic!("mix_down {mix_down}: {error}"));
        assert_eq!(encoded.len(), tracks, "mix_down {mix_down}");
        for track in &encoded {
            assert_eq!(track.rate, RATE);
            let out = seconds(track);
            assert!((out - expected).abs() < 0.1, "mix_down {mix_down}: {out} s of audio for {expected} s");
        }
    }
}
