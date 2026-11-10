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

/// `bytes` as a stem file in a scratch folder, opened as the mixer opens stems.
fn open_wav(name: &str, bytes: &[u8]) -> Result<stem::StemReader, String> {
    let dir = crate::test_temp::TempDir::new(format!("recall-wav-{name}-{}", std::process::id()));
    let path = dir.path().join("audio.wav");
    std::fs::write(&path, bytes).unwrap();
    stem::StemReader::open(&path.to_string_lossy())
}

#[test]
fn parses_pcm16_with_unknown_chunks() {
    let mut wav = b"RIFF\0\0\0\0WAVEJUNK\x02\0\0\0xxfmt \x10\0\0\0\x01\0\x01\0\x80\xbb\0\0\0w\x01\0\x02\0\x10\0data\x04\0\0\0\0\x80\xff\x7f".to_vec();
    let size = (wav.len() - 8) as u32;
    wav[4..8].copy_from_slice(&size.to_le_bytes());
    let mut stem = open_wav("chunks", &wav).unwrap();
    assert_eq!((stem.channels(), stem.frames()), (1, 2));
    let mut samples = Vec::new();
    stem.read(0, 3, &mut samples).unwrap();
    assert_eq!(samples[0], -1.0);
    assert!(samples[1] > 0.99);
    assert_eq!(samples[2], 0.0, "past the end: silence");
}

#[test]
fn mixes_gain_mute_and_mono_to_stereo() {
    let mut tracks = vec![
        LoadedTrack {
            stream_index: 0,
            samples: Samples::Memory(vec![0.5]),
            channels: 1,
            gain_db: 0.0,
            muted: false,
        },
        LoadedTrack {
            stream_index: 1,
            samples: Samples::Memory(vec![1.0, 1.0]),
            channels: 2,
            gain_db: -6.0206,
            muted: false,
        },
        LoadedTrack {
            stream_index: 2,
            samples: Samples::Memory(vec![1.0]),
            channels: 1,
            gain_db: 0.0,
            muted: true,
        },
    ];
    let mut output = [0i16; 2];
    mix_frames(&mut tracks, 0, 1, &mut output).unwrap();
    assert!((output[0] as i32 - 32767).abs() < 4);
    assert!((output[1] as i32 - 32767).abs() < 4);
}

#[test]
fn a_silent_buffer_carries_no_sound_and_a_normal_one_does() {
    let mut tracks = [LoadedTrack { stream_index: 0, samples: Samples::Memory(vec![0.5; 4]), channels: 1, gain_db: 0.0, muted: false }];
    let mut output = vec![1i16; FRAMES_PER_BUFFER * OUTPUT_CHANNELS];
    worker::fill_buffer(&mut tracks, 0, 2, false, &mut output).unwrap();
    assert!(output[0] > 16000);
    assert!(output[4..].iter().all(|s| *s == 0), "past the frames: padded");
    worker::fill_buffer(&mut tracks, 0, 2, true, &mut output).unwrap();
    assert!(output.iter().all(|s| *s == 0));
}

#[test]
fn rejects_malformed_truncated_and_non_48k_wav() {
    assert!(open_wav("garbage", b"not a wave").is_err());
    let mut truncated = pcm16_wav(SAMPLE_RATE, &[0, 0]);
    truncated.truncate(truncated.len() - 1);
    assert!(open_wav("truncated", &truncated).is_err());
    let non_48k = pcm16_wav(44_100, &[0, 0]);
    assert!(open_wav("44k", &non_48k).is_err());
}

#[test]
fn clips_mixed_output() {
    let mut tracks = vec![LoadedTrack {
        stream_index: 0,
        samples: Samples::Memory(vec![1.0, -1.0]),
        channels: 2,
        gain_db: 24.0,
        muted: false,
    }];
    let mut output = [0i16; 2];
    mix_frames(&mut tracks, 0, 1, &mut output).unwrap();
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
    if !crate::hw_gate::enabled("audio_soak") {
        return;
    }
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
fn a_mute_set_while_the_tracks_load_is_queued_not_dropped() {
    // 2026-11-04: set_audio refused until ready, so a mute pressed while the Mic track loaded
    // showed as muted in the editor but still played.
    let (mixer, rx) = detached_mixer(5.0);
    mixer.shared.ready.store(false, Ordering::Release);
    mixer.set_audio(2, -6.0, true).unwrap();
    match rx.try_recv() {
        Ok(Command::SetAudio { stream_index, gain_db, muted }) => assert_eq!((stream_index, gain_db, muted), (2, -6.0, true)),
        _ => panic!("the mute must be queued for the worker"),
    }
    *mixer.shared.error.lock().unwrap() = Some("broken".into());
    assert!(mixer.set_audio(2, 0.0, false).is_err(), "a failed preview still reports its error");
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

/// FFmpeg's FLAC of `seconds` of a stereo 48 kHz sine (left `hz`, right silent), as the engine
/// writes stems; None when ffmpeg is not found.
pub(super) fn flac_stem(dir: &std::path::Path, name: &str, seconds: u32, hz: u32) -> Option<String> {
    let ffmpeg = crate::services::storage::binary("RECALL_FFMPEG", "ffmpeg.exe").unwrap_or_else(|_| "ffmpeg.exe".into());
    let path = dir.join(name);
    let filter = format!("sine=frequency={hz}:sample_rate=48000:duration={seconds},pan=stereo|c0=c0|c1=0*c0");
    let made = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &filter, "-c:a", "flac"])
        .arg(&path)
        .status();
    made.is_ok_and(|status| status.success()).then(|| path.to_string_lossy().into_owned())
}

#[test]
fn three_five_minute_stereo_stems_open_without_a_size_cliff() {
    // E1 (deep audit 2026-11-07): 3 stereo tracks over 233 s passed the 256 MiB decoded limit,
    // so the preview failed and a pointless proxy transcode ran.
    let dir = crate::test_temp::TempDir::new(format!("recall-stem-cliff-{}", std::process::id()));
    let stems: Option<Vec<String>> = (0..3).map(|i| flac_stem(dir.path(), &format!("audio-{i}.flac"), 300, 440 + i * 110)).collect();
    let Some(stems) = stems else {
        eprintln!("ffmpeg not found; skipped");
        return;
    };
    let specs: Vec<PreviewTrack> = stems.iter().enumerate().map(|(i, path)| PreviewTrack { stream_index: i + 1, path: path.clone(), gain_db: 0.0, muted: false }).collect();
    let loaded = load_tracks(&specs).expect("15 minutes of stems open");
    assert_eq!(loaded.len(), 3);
    assert!(loaded.iter().all(|track| track.samples.len() / track.channels == 300 * 48_000), "every stem is 5 minutes long");
    // The last second plays: the sound is read from the files as it is needed.
    let mut tracks = loaded;
    let mut output = vec![0i16; FRAMES_PER_BUFFER * OUTPUT_CHANNELS];
    mix_frames(&mut tracks, 299 * 48_000, FRAMES_PER_BUFFER, &mut output).unwrap();
    assert!(output.iter().step_by(2).any(|s| s.unsigned_abs() > 1000), "the tones at 4:59 are heard");
}
