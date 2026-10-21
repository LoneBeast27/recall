//! Tests for `audio.rs`: WAV parsing, mixing and stale-generation errors.
use super::*;

fn pcm16_wav(sample_rate: u32, data: &[u8]) -> Vec<u8> {
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(data);
    wav
}

#[test]
fn parses_pcm16_with_unknown_chunks() {
    let mut wav = b"RIFF\0\0\0\0WAVEJUNK\x02\0\0\0xxfmt \x10\0\0\0\x01\0\x01\0\x80\xbb\0\0\0w\x01\0\x02\0\x10\0data\x04\0\0\0\0\x80\xff\x7f".to_vec();
    let size = (wav.len() - 8) as u32;
    wav[4..8].copy_from_slice(&size.to_le_bytes());
    let (samples, channels) = parse_wav(&wav, MAX_DECODED_BYTES).unwrap();
    assert_eq!(channels, 1);
    assert_eq!(samples.len(), 2);
    assert_eq!(samples[0], -1.0);
    assert!(samples[1] > 0.99);
}

#[test]
fn mixes_gain_mute_and_mono_to_stereo() {
    let tracks = vec![
        LoadedTrack {
            stream_index: 0,
            samples: vec![0.5],
            channels: 1,
            gain_db: 0.0,
            muted: false,
        },
        LoadedTrack {
            stream_index: 1,
            samples: vec![1.0, 1.0],
            channels: 2,
            gain_db: -6.0206,
            muted: false,
        },
        LoadedTrack {
            stream_index: 2,
            samples: vec![1.0],
            channels: 1,
            gain_db: 0.0,
            muted: true,
        },
    ];
    let mut output = [0i16; 2];
    mix_frames(&tracks, 0, 1, &mut output);
    assert!((output[0] as i32 - 32767).abs() < 4);
    assert!((output[1] as i32 - 32767).abs() < 4);
}

#[test]
fn rejects_malformed_truncated_and_non_48k_wav() {
    assert!(parse_wav(b"not a wave", MAX_DECODED_BYTES).is_err());
    let mut truncated = pcm16_wav(SAMPLE_RATE, &[0, 0]);
    truncated.truncate(truncated.len() - 1);
    assert!(parse_wav(&truncated, MAX_DECODED_BYTES).is_err());
    let non_48k = pcm16_wav(44_100, &[0, 0]);
    assert!(parse_wav(&non_48k, MAX_DECODED_BYTES).is_err());
}

#[test]
fn clips_mixed_output() {
    let tracks = vec![LoadedTrack {
        stream_index: 0,
        samples: vec![1.0, -1.0],
        channels: 2,
        gain_db: 24.0,
        muted: false,
    }];
    let mut output = [0i16; 2];
    mix_frames(&tracks, 0, 1, &mut output);
    assert_eq!(output, [i16::MAX, -i16::MAX]);
}

#[test]
fn stale_generation_cannot_publish_error() {
    let status = SharedStatus::default();
    status.generation.store(2, Ordering::Release);
    status.fail_if_current(1, "stale");
    assert!(status.error.lock().unwrap().is_none());
    status.fail_if_current(2, "current");
    assert_eq!(status.error.lock().unwrap().as_deref(), Some("current"));
}
