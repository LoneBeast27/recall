use super::*;
use crate::export::test_media::{synth, Spec};

/// R2-deep-editor-5: a decode error from `receive_frame` is skipped like a rejected packet
/// (it failed the whole export before), and the stream still counts as broken after 50 in a row.
#[test]
fn a_decode_error_on_the_receive_side_is_skipped_like_a_bad_packet() {
    let Some(clip) = synth("reader-receive-error", &Spec { tones: vec![440.0], seconds: 2.0, ..Spec::default() }) else {
        eprintln!("libx264 missing in this FFmpeg build; skipped");
        return;
    };
    let index = crate::edit_audio::audio_indices(&clip.path).unwrap()[0];
    let mut reader = TrackReader::open(&clip.path, 0, &[TimeRange::new(0.0, 1.5)], index, None, None).unwrap();
    reader.decode.receive_failed(ff::Error::InvalidData).expect("one lost frame is skipped");
    assert!(reader.decode.reseed, "the next frame goes to its own timestamp");
    reader.fill(48_000).unwrap();
    assert!(!reader.decode.reseed, "and it did");
    assert!(reader.available() >= 48_000, "decoding carried on");
    for _ in 0..49 {
        reader.decode.receive_failed(ff::Error::InvalidData).unwrap();
    }
    let broken = reader.decode.receive_failed(ff::Error::InvalidData).unwrap_err();
    assert!(broken.contains("50 undecodable packets in a row"), "{broken}");
}
