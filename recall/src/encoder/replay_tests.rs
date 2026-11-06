use super::*;
use crate::ring::Payload;

fn cfg() -> Config {
    Config { fps: 10, buffer_secs: 60, bitrate: 1_000_000, ..Config::from_env(false) }
}

fn seg(w: u32, sps: &[u8]) -> Segment {
    Segment { start: None, params: OwnedParams::test_h264(w as i32, 360, sps), backend: "h264_nvenc", codec: Codec::H264, width: w, height: 360 }
}

/// `secs` of 10 fps packets from `from_s`, a keyframe each second.
fn packets(replay: &mut Replay, from_s: u64, secs: u64) {
    for i in 0..secs * 10 {
        let pts = from_s * 1_000_000 + i * 100_000;
        let data: Arc<[u8]> = Arc::from(&[1u8, 2, 3][..]);
        replay.push(EncodedPacket { frame_idx: pts, duration_us: 100_000, keyframe: i % 10 == 0, data: Payload::from(data) });
    }
}

fn window_secs(replay: &Replay) -> f64 {
    let end = replay.ring.end_us().unwrap();
    let (packets, _) = replay.window(60_000_000, end).unwrap();
    super::super::saver::window_secs(&packets)
}

#[test]
fn an_encoder_reopened_with_the_same_stream_keeps_the_whole_buffer() {
    let mut replay = Replay::new(&cfg(), None);
    replay.begin_segment(seg(640, b"sps-a"));
    packets(&mut replay, 0, 5);
    // The encoder failed at 5 s; its replacement opens at 7 s with the same parameter sets.
    replay.begin_segment(seg(640, b"sps-a"));
    packets(&mut replay, 7, 3);
    let secs = window_secs(&replay);
    assert!(secs > 9.9, "both encoders' packets and the held frame between: {secs}");
}

#[test]
fn a_new_stream_cuts_the_save_at_its_first_keyframe() {
    let mut replay = Replay::new(&cfg(), None);
    replay.begin_segment(seg(640, b"sps-a"));
    packets(&mut replay, 0, 5);
    replay.begin_segment(seg(640, b"sps-b"));
    packets(&mut replay, 7, 3);
    let end = replay.ring.end_us().unwrap();
    let (window, stream) = replay.window(60_000_000, end).unwrap();
    assert!(window[0].keyframe);
    assert!((super::super::saver::window_secs(&window) - 3.0).abs() < 0.11, "only the newest encoder's 3 s");
    assert!(stream.params.same_stream(&OwnedParams::test_h264(640, 360, b"sps-b")), "muxed with the newest parameters");
}

#[test]
fn a_save_in_the_gap_uses_the_old_encoders_packets() {
    let mut replay = Replay::new(&cfg(), None);
    replay.begin_segment(seg(640, b"sps-a"));
    packets(&mut replay, 0, 5);
    // A replacement opened but has not written a packet yet.
    replay.begin_segment(seg(640, b"sps-b"));
    assert!((window_secs(&replay) - 5.0).abs() < 0.11);
}

#[test]
fn evicted_segments_are_forgotten_and_a_new_size_is_reported() {
    let mut replay = Replay::new(&Config { buffer_secs: 4, ..cfg() }, None);
    replay.begin_segment(seg(640, b"sps-a"));
    packets(&mut replay, 0, 3);
    replay.begin_segment(seg(640, b"sps-b"));
    packets(&mut replay, 3, 6);
    assert_eq!(replay.segments.len(), 1, "the first encoder's packets left the ring");
    replay.begin_segment(seg(800, b"sps-c"));
    assert_eq!(replay.size(), Some((800, 360)));
}
