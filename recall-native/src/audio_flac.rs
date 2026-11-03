//! FLAC editor stems (the engine writes them from stem cache version 6; WAV before) decoded to
//! the interleaved f32 samples the WAV path gives, within the same decoded-size budget.
//! Pure Rust (symphonia's FLAC bundle): no codec needs to be installed on the machine.
use std::io::Cursor;

use symphonia_bundle_flac::{FlacDecoder, FlacReader};
use symphonia_core::audio::SampleBuffer;
use symphonia_core::codecs::{Decoder, DecoderOptions};
use symphonia_core::errors::Error;
use symphonia_core::formats::{FormatOptions, FormatReader};
use symphonia_core::io::MediaSourceStream;

pub(crate) fn is_flac(bytes: &[u8]) -> bool {
    bytes.starts_with(b"fLaC")
}

/// (interleaved samples, channels) of a mono or stereo FLAC at `rate`.
pub(crate) fn parse_flac(bytes: &[u8], decoded_budget: usize, rate: u32) -> Result<(Vec<f32>, usize), String> {
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let mut reader = FlacReader::try_new(source, &FormatOptions::default()).map_err(|e| format!("read FLAC: {e}"))?;
    let params = reader.default_track().ok_or("FLAC has no audio")?.codec_params.clone();
    let channels = params.channels.map_or(0, |c| c.count());
    if !(channels == 1 || channels == 2) {
        return Err(format!("expected mono or stereo FLAC, got {channels} channels"));
    }
    if params.sample_rate != Some(rate) {
        return Err(format!("expected {rate} Hz FLAC, got {:?} Hz", params.sample_rate));
    }
    let mut decoder = FlacDecoder::try_new(&params, &DecoderOptions::default()).map_err(|e| format!("FLAC decoder: {e}"))?;
    let mut samples: Vec<f32> = Vec::new();
    let mut buffer: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match reader.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("read FLAC: {e}")),
        };
        let decoded = decoder.decode(&packet).map_err(|e| format!("decode FLAC: {e}"))?;
        let needed = decoded.capacity() * channels;
        if buffer.as_ref().is_none_or(|b| b.capacity() < needed) {
            buffer = Some(SampleBuffer::new(decoded.capacity() as u64, *decoded.spec()));
        }
        let buffer = buffer.as_mut().expect("set above");
        buffer.copy_interleaved_ref(decoded);
        if (samples.len() + buffer.samples().len()) * size_of::<f32>() > decoded_budget {
            return Err("decoded preview audio exceeds the 256 MiB pilot limit".into());
        }
        samples.extend_from_slice(buffer.samples());
    }
    Ok((samples, channels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A FLAC made by FFmpeg (as the engine does) decodes to the same samples as its WAV.
    #[test]
    fn a_flac_stem_decodes_to_the_samples_of_its_wav() {
        let dir = std::env::temp_dir().join(format!("recall-flac-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wav, flac) = (dir.join("tone.wav"), dir.join("tone.flac"));
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
        let made = Command::new(ffmpeg).args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&wav).args(["-c:a", "flac"]).arg(&flac).status();
        if !made.is_ok_and(|status| status.success()) {
            eprintln!("ffmpeg not found; skipped");
            return;
        }
        let bytes = std::fs::read(&flac).unwrap();
        assert!(is_flac(&bytes));
        assert!(bytes.len() < file.len(), "smaller than the WAV: {} vs {}", bytes.len(), file.len());
        let (decoded, channels) = parse_flac(&bytes, usize::MAX, 48_000).unwrap();
        let (expected, _) = super::super::parse_wav_for_tests(&file);
        assert_eq!(channels, 2);
        assert_eq!(decoded, expected, "lossless");
        assert!(parse_flac(&bytes, 1000, 48_000).is_err(), "the decoded-size budget holds");
        assert!(parse_flac(&bytes, usize::MAX, 44_100).is_err(), "only 48 kHz stems");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
