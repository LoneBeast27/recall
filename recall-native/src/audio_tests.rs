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

/// Playback-path soak (run by hand, plays silence so nobody hears it):
/// `RECALL_AUDIO_SOAK=<folder of audio-*.wav stems> cargo test audio_soak -- --ignored --nocapture`
/// (`RECALL_AUDIO_SOAK_SECS`, default 30). Plays the stems muted on the default device and
/// reports output underruns, how far the sample clock fell behind the wall clock, and how fast
/// sound resumes after a pause.
#[test]
#[ignore]
fn audio_soak() {
    use std::time::{Duration, Instant};
    let Some(dir) = std::env::var_os("RECALL_AUDIO_SOAK") else { return };
    let secs: f64 = std::env::var("RECALL_AUDIO_SOAK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let mut stems: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "wav")).collect();
    stems.sort();
    let tracks: Vec<PreviewTrack> = stems
        .iter()
        .enumerate()
        .map(|(i, p)| PreviewTrack { stream_index: i + 1, path: p.to_string_lossy().into_owned(), gain_db: 0.0, muted: true })
        .collect();
    let mixer = AudioMixer::new().unwrap();
    mixer.open(&tracks, 0.0).unwrap();
    let t = Instant::now();
    while !mixer.is_ready() {
        assert!(mixer.error().is_none(), "{:?}", mixer.error());
        assert!(t.elapsed() < Duration::from_secs(20), "stems never loaded");
        std::thread::sleep(Duration::from_millis(10));
    }
    let wait_moving = |from: f64| {
        let t = Instant::now();
        while mixer.position() <= from + 0.001 && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(1));
        }
        t.elapsed()
    };
    mixer.play().unwrap();
    let first = wait_moving(0.0);
    let (start_pos, start) = (mixer.position(), Instant::now());
    let mut worst_lag = 0.0f64;
    while start.elapsed().as_secs_f64() < secs {
        std::thread::sleep(Duration::from_millis(50));
        let lag = start.elapsed().as_secs_f64() - (mixer.position() - start_pos);
        worst_lag = worst_lag.max(lag);
    }
    let played = mixer.position() - start_pos;
    let wall = start.elapsed().as_secs_f64();
    let mut resumes = Vec::new();
    for _ in 0..10 {
        mixer.pause().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let at = mixer.position();
        mixer.play().unwrap();
        resumes.push(wait_moving(at).as_millis());
        std::thread::sleep(Duration::from_millis(200));
    }
    mixer.pause().unwrap();
    eprintln!(
        "audio soak: {} stems, first sound {} ms, played {played:.3} s in {wall:.3} s wall (clock behind by {:.0} ms at the end, worst {:.0} ms), underruns {}, resume after pause (ms) {resumes:?}",
        tracks.len(),
        first.as_millis(),
        (wall - played) * 1000.0,
        worst_lag * 1000.0,
        mixer.underruns(),
    );
}

/// A mixer whose worker never runs: the test plays the worker's part by hand.
fn detached_mixer(clock_seconds: f64) -> (AudioMixer, mpsc::Receiver<Command>) {
    let (tx, rx) = mpsc::channel();
    let shared = Arc::new(SharedStatus::default());
    shared.ready.store(true, Ordering::Release);
    shared.duration_frames.store((clock_seconds * SAMPLE_RATE as f64) as u64, Ordering::Release);
    let mixer = AudioMixer { tx, shared, worker: None, sent: Cell::new(0), seek_sent: Cell::new((0, 0.0)), run_sent: Cell::new((0, false)) };
    (mixer, rx)
}

#[test]
fn a_queued_seek_reads_as_done_until_the_worker_catches_up() {
    // 2026-10-30: play pressed at the end ran "seek to start, play" back to back; play read the
    // old clock (the seek still queued) and put the picture back at the end.
    let (mixer, rx) = detached_mixer(10.453);
    mixer.shared.set_position(10.453);
    mixer.seek(0.0).unwrap();
    assert_eq!(mixer.position(), 0.0, "the queued seek's target, not the stale clock");
    mixer.play().unwrap();
    assert!(mixer.is_playing(), "play is queued behind the seek");
    assert_eq!(mixer.position(), 0.0);
    assert_eq!(rx.try_iter().count(), 2);
    // The worker carried out both: the shared clock is the truth again.
    mixer.shared.set_position(0.25);
    mixer.shared.playing.store(true, Ordering::Release);
    mixer.shared.applied.store(2, Ordering::Release);
    assert_eq!(mixer.position(), 0.25);
    assert!(mixer.is_playing());
    mixer.pause().unwrap();
    assert!(!mixer.is_playing(), "a queued pause reads as paused");
}

#[test]
fn a_queued_seek_is_clamped_to_the_clock_and_a_seek_to_the_end_stops() {
    let (mixer, _rx) = detached_mixer(10.453);
    mixer.play().unwrap();
    mixer.shared.playing.store(true, Ordering::Release);
    mixer.shared.applied.store(1, Ordering::Release);
    mixer.seek(99.0).unwrap();
    assert!((mixer.position() - 10.453).abs() < 1e-3);
    assert!(!mixer.is_playing(), "the sound has nothing left to play");
    mixer.seek(3.0).unwrap();
    assert_eq!(mixer.position(), 3.0);
    assert!(!mixer.is_playing(), "a seek keeps the paused state");
}

#[test]
fn the_clock_covers_the_whole_clip_when_stems_end_early() {
    // Sound 9.9 s, picture 10.438 s: the clock (which moves the picture) runs to 10.438.
    assert_eq!(worker::clock_frames(475_200, 10.437942), 501_021);
    // Stems longer than the clip keep their length; an unknown clip length changes nothing.
    assert_eq!(worker::clock_frames(501_744, 10.0), 501_744);
    assert_eq!(worker::clock_frames(1_000, 0.0), 1_000);
    assert_eq!(worker::clock_frames(1_000, f64::NAN), 1_000);
}
