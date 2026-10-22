//! Audible feedback for in-game actions: a soft tick the moment a save / bookmark / screenshot
//! is accepted, a two-note chime when a clip is on disk, a low tone on failure.
//!
//! The sounds are synthesized WAVs played from memory (`PlaySoundW`, async), so they work
//! with the overlay off and do not depend on the Windows sound scheme (the old `MessageBeep`
//! cues were silent under "No sounds", and a 2-minute save to a hard drive takes seconds, so
//! the only cue arrived late).
use std::sync::OnceLock;

const RATE: u32 = 48_000;

/// (frequency Hz, duration ms) notes, played back to back with a short decay each.
type Notes = &'static [(f32, u32)];
const TICK: Notes = &[(1320.0, 45)];
const SAVED: Notes = &[(880.0, 70), (1320.0, 120)];
const FAILED: Notes = &[(330.0, 110), (220.0, 160)];

/// Accepted: a save, bookmark, screenshot or recording start was sent to the engine.
pub fn tick() {
    play(&TICK_WAV, TICK, 0.22);
}

/// A clip or long recording is on disk.
pub fn saved() {
    play(&SAVED_WAV, SAVED, 0.28);
}

/// A save / screenshot / recording failed or was refused.
pub fn failed() {
    play(&FAILED_WAV, FAILED, 0.3);
}

static TICK_WAV: OnceLock<Vec<u8>> = OnceLock::new();
static SAVED_WAV: OnceLock<Vec<u8>> = OnceLock::new();
static FAILED_WAV: OnceLock<Vec<u8>> = OnceLock::new();

fn play(cell: &'static OnceLock<Vec<u8>>, notes: Notes, volume: f32) {
    // The buffer lives for the whole process, as SND_MEMORY|SND_ASYNC requires.
    let wav = cell.get_or_init(|| wav(notes, volume));
    #[link(name = "winmm")]
    unsafe extern "system" {
        fn PlaySoundW(sound: *const u16, module: *mut core::ffi::c_void, flags: u32) -> i32;
    }
    const SND_ASYNC: u32 = 0x0001;
    const SND_NODEFAULT: u32 = 0x0002;
    const SND_MEMORY: u32 = 0x0004;
    unsafe {
        let _ = PlaySoundW(wav.as_ptr().cast(), std::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

/// 16-bit mono PCM WAV of the notes, each with a 4 ms attack and an exponential decay.
fn wav(notes: Notes, volume: f32) -> Vec<u8> {
    let mut samples: Vec<i16> = Vec::new();
    for &(freq, ms) in notes {
        let n = (RATE * ms / 1000) as usize;
        let attack = (RATE as usize * 4) / 1000;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let envelope = (i as f32 / attack as f32).min(1.0) * (-5.0 * i as f32 / n as f32).exp();
            let value = (2.0 * std::f32::consts::PI * freq * t).sin() * envelope * volume;
            samples.push((value * i16::MAX as f32) as i16);
        }
    }
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_and_length_match_the_notes() {
        let bytes = wav(SAVED, 0.28);
        let samples = (RATE * 70 / 1000 + RATE * 120 / 1000) as usize;
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize, samples * 2);
        assert_eq!(bytes.len(), 44 + samples * 2);
    }

    #[test]
    fn samples_stay_inside_the_volume() {
        let bytes = wav(FAILED, 0.3);
        let peak = bytes[44..].chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs()).max().unwrap();
        assert!(peak > 0 && f32::from(peak) <= 0.3 * i16::MAX as f32 + 1.0);
    }
}
