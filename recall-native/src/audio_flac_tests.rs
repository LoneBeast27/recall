//! Tests for `audio_flac.rs`: FLAC stems read from disk in blocks, in any order.
use super::super::stem::StemReader;
use super::super::tests::flac_stem;

/// Every frame of `stem`, read front to back in device-buffer blocks.
fn read_all(stem: &mut StemReader) -> Vec<f32> {
    let (mut all, mut block) = (Vec::new(), Vec::new());
    let mut at = 0;
    while at < stem.frames() {
        stem.read(at, 960, &mut block).unwrap();
        all.extend_from_slice(&block);
        at += 960;
    }
    all.truncate(stem.frames() * stem.channels());
    all
}

#[test]
fn a_flac_stem_decodes_to_the_samples_of_its_wav() {
    let dir = crate::test_temp::TempDir::new(format!("recall-flac-{}", std::process::id()));
    let (wav, flac) = (dir.path().join("tone.wav"), dir.path().join("tone.flac"));
    let mut pcm = Vec::new();
    for i in 0..48_000u32 {
        let value = ((i as f32 * 440.0 / 48_000.0 * std::f32::consts::TAU).sin() * 12_000.0) as i16;
        pcm.extend_from_slice(&value.to_le_bytes());
        pcm.extend_from_slice(&(-value).to_le_bytes());
    }
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x02\0\x80\xbb\0\0\0\xee\x02\0\x04\0\x10\0data");
    file.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    file.extend_from_slice(&pcm);
    std::fs::write(&wav, &file).unwrap();
    let ffmpeg = crate::services::storage::binary("RECALL_FFMPEG", "ffmpeg.exe").unwrap_or_else(|_| "ffmpeg.exe".into());
    let made = std::process::Command::new(ffmpeg).args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&wav).args(["-c:a", "flac"]).arg(&flac).status();
    if !made.is_ok_and(|status| status.success()) {
        eprintln!("ffmpeg not found; skipped");
        return;
    }
    let mut from_flac = StemReader::open(&flac.to_string_lossy()).unwrap();
    let mut from_wav = StemReader::open(&wav.to_string_lossy()).unwrap();
    assert!(matches!(from_flac, StemReader::Flac(_)) && matches!(from_wav, StemReader::Wav(_)));
    assert_eq!((from_flac.channels(), from_flac.frames()), (2, 48_000));
    assert_eq!(read_all(&mut from_flac), read_all(&mut from_wav), "lossless");
}

#[test]
fn reads_in_any_order_match_a_front_to_back_read() {
    let dir = crate::test_temp::TempDir::new(format!("recall-flac-seek-{}", std::process::id()));
    let Some(path) = flac_stem(dir.path(), "tone.flac", 20, 440) else {
        eprintln!("ffmpeg not found; skipped");
        return;
    };
    let mut stem = StemReader::open(&path).unwrap();
    let whole = read_all(&mut stem);
    assert_eq!(whole.len(), 20 * 48_000 * 2);
    let mut block = Vec::new();
    // Back, a jump ahead, a short step ahead, across the end, and past it.
    for &(from, count) in &[(3 * 48_000 + 17, 960), (0, 960), (15 * 48_000 + 5, 2048), (15 * 48_000 + 30_000, 960), (20 * 48_000 - 100, 960), (25 * 48_000, 960), (7, 4096)] {
        stem.read(from, count, &mut block).unwrap();
        assert_eq!(block.len(), count * 2);
        let inside = (20 * 48_000usize).saturating_sub(from).min(count) * 2;
        if inside > 0 {
            assert_eq!(&block[..inside], &whole[from * 2..from * 2 + inside], "frames {from}+{count}");
        }
        assert!(block[inside..].iter().all(|s| *s == 0.0), "silence past the end at {from}");
    }
}
