//! Codec mapping, fallback and env parsing (pure, no FFmpeg).

use super::*;
use std::collections::HashMap;

fn knobs(pairs: &[(&str, &str)]) -> Knobs {
    let env: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    from_lookup(|key| env.get(key).cloned())
}

#[test]
fn codec_names_map_to_nvenc_encoders_and_wire_labels() {
    let cases = [
        ("h264", Codec::H264, "h264_nvenc", "h264"),
        ("H.264", Codec::H264, "h264_nvenc", "h264"),
        ("hevc", Codec::Hevc, "hevc_nvenc", "hevc"),
        ("H265", Codec::Hevc, "hevc_nvenc", "hevc"),
        (" AV1 ", Codec::Av1, "av1_nvenc", "av1"),
    ];
    for (raw, codec, encoder, label) in cases {
        assert_eq!(Codec::parse(raw), Some(codec), "{raw}");
        assert_eq!((codec.nvenc(), codec.label()), (encoder, label));
        assert_eq!(Codec::from_index(codec.index()), codec);
    }
    assert_eq!(Codec::parse("vp9"), None);
    assert_eq!(Codec::from_index(0), Codec::H264, "unset stats read as h264");
}

#[test]
fn fallback_chain_ends_in_h264_once() {
    assert_eq!(fallback_chain(Codec::H264), vec![Codec::H264]);
    assert_eq!(fallback_chain(Codec::Hevc), vec![Codec::Hevc, Codec::H264]);
    assert_eq!(fallback_chain(Codec::Av1), vec![Codec::Av1, Codec::H264]);
}

#[test]
fn failed_codec_falls_back_to_h264_with_a_log_line() {
    let mut tried = Vec::new();
    let opened = open_with_fallback(Codec::Av1, |c| {
        tried.push(c);
        match c {
            Codec::Av1 => Err("codec av1_nvenc not found".to_string()),
            other => Ok(other.nvenc()),
        }
    })
    .unwrap();
    assert_eq!(tried, vec![Codec::Av1, Codec::H264]);
    assert_eq!((opened.codec, opened.value), (Codec::H264, "h264_nvenc"));
    let note = opened.fallback.expect("fallback must be reported");
    assert!(note.contains("av1_nvenc unavailable"), "{note}");
    assert!(note.contains("codec av1_nvenc not found"), "{note}");
    assert!(note.ends_with("recording with h264_nvenc instead"), "{note}");
}

#[test]
fn requested_codec_that_opens_reports_no_fallback() {
    let opened = open_with_fallback(Codec::Hevc, |c| Ok::<_, String>(c)).unwrap();
    assert_eq!(opened.codec, Codec::Hevc);
    assert!(opened.fallback.is_none());
}

#[test]
fn every_codec_failing_is_an_error_naming_each_attempt() {
    let err = open_with_fallback(Codec::Hevc, |c| Err::<(), _>(format!("{} busy", c.label())))
        .err()
        .unwrap();
    assert_eq!(
        err,
        "NVENC initialization failed: hevc_nvenc: hevc busy; h264_nvenc: h264 busy"
    );
}

#[test]
fn quality_presets_keep_bf_zero_and_map_preset_and_bitrate() {
    for q in [Quality::Balanced, Quality::High, Quality::Max] {
        assert!(q.nvenc_options().contains(&("bf", "0")), "{q:?} must keep bf=0");
        assert!(q.nvenc_options().contains(&("tune", "hq")));
        assert_eq!(Quality::parse(q.label()), Some(q));
    }
    let preset = |q: Quality| q.nvenc_options().iter().find(|o| o.0 == "preset").unwrap().1;
    assert_eq!(
        [preset(Quality::Balanced), preset(Quality::High), preset(Quality::Max)],
        ["p4", "p5", "p7"]
    );
    assert!(Quality::Max.nvenc_options().contains(&("multipass", "qres")));
    assert!(!Quality::High.nvenc_options().iter().any(|o| o.0 == "multipass"));
    assert_eq!(Quality::High.default_bitrate(), 25_000_000, "today's default");
}

#[test]
fn defaults_reproduce_todays_recorder() {
    let k = knobs(&[]);
    assert_eq!(
        (k.codec, k.quality, k.fps, k.bitrate, k.buffer_secs),
        (Codec::H264, Quality::High, 60, 25_000_000, 60)
    );
    assert!(k.notes.is_empty());
}

#[test]
fn bitrate_accepts_mbps_kbps_and_bps() {
    let cases = [
        ("25", 25_000_000),
        ("12.5", 12_500_000),
        ("25M", 25_000_000),
        ("25 Mbps", 25_000_000),
        ("8000k", 8_000_000),
        ("8000kbps", 8_000_000),
        ("25000000", 25_000_000),
        ("1000", 1_000_000_000),
    ];
    for (raw, bps) in cases {
        assert_eq!(parse_bitrate(raw), Some(bps), "{raw}");
    }
    for bad in ["", "fast", "-5", "0", "NaN", "inf"] {
        assert_eq!(parse_bitrate(bad), None, "{bad}");
    }
}

#[test]
fn env_values_are_applied_and_bad_ones_noted() {
    let k = knobs(&[
        ("RECALL_CODEC", "hevc"),
        ("RECALL_QUALITY", "max"),
        ("RECALL_FPS", "144"),
        ("RECALL_BUFFER_SECS", "120"),
        ("RECALL_BITRATE", "50"),
    ]);
    assert_eq!(
        (k.codec, k.quality, k.fps, k.bitrate, k.buffer_secs),
        (Codec::Hevc, Quality::Max, 144, 50_000_000, 120)
    );
    assert!(k.notes.is_empty(), "{:?}", k.notes);

    // Quality picks the bitrate only while RECALL_BITRATE is unset.
    assert_eq!(knobs(&[("RECALL_QUALITY", "balanced")]).bitrate, 15_000_000);

    let k = knobs(&[
        ("RECALL_CODEC", "vp9"),
        ("RECALL_FPS", "0"),
        ("RECALL_BUFFER_SECS", "lots"),
        ("RECALL_BITRATE", "900"),
    ]);
    assert_eq!(
        (k.codec, k.fps, k.buffer_secs, k.bitrate),
        (Codec::H264, 60, 60, 500_000_000)
    );
    assert_eq!(k.notes.len(), 4, "{:?}", k.notes);
    assert!(k.notes.iter().any(|n| n.starts_with("RECALL_CODEC \"vp9\"")));
    assert!(k.notes.iter().any(|n| n.contains("outside 1..=500 Mbps")));

    // Out-of-range values clamp to the nearest bound.
    let k = knobs(&[
        ("RECALL_FPS", "5000"),
        ("RECALL_BUFFER_SECS", "2"),
        ("RECALL_BITRATE", "9000"), // 9000 bps
    ]);
    assert_eq!((k.fps, k.buffer_secs, k.bitrate), (1000, 5, 1_000_000));
    assert_eq!(k.notes.len(), 3, "{:?}", k.notes);
}

#[test]
fn a_low_end_gpu_caps_nvenc_effort_at_p4() {
    let preset = |q: Quality| q.nvenc_options().iter().find(|o| o.0 == "preset").unwrap().1;
    for q in [Quality::Balanced, Quality::High, Quality::Max] {
        assert_eq!(preset(q.for_gpu(true)), "p4", "{q:?}");
        assert!(!q.for_gpu(true).nvenc_options().iter().any(|o| o.0 == "multipass"));
        assert_eq!(q.for_gpu(false), q);
    }
}

#[test]
fn an_av1_session_that_falls_back_ends_on_h264_within_its_size_limit() {
    use crate::framing::{capped_within, H264_MAX_SIDE};
    // A GPU without AV1, whose H.264 takes 4096 a side; a 5120x1440 window.
    let open = |codec: Codec, (w, h): (u32, u32)| match codec {
        Codec::H264 if w.max(h) <= H264_MAX_SIDE => Ok((w, h)),
        Codec::H264 => Err("Function not implemented".to_string()),
        _ => Err("no AV1 on this GPU".to_string()),
    };
    let window = (5120, 1440);
    let mut limit = 8192;
    let mut opened = None;
    for _ in 0..2 {
        let size = capped_within(window, None, limit);
        let result = open_with_fallback(Codec::Av1, |c| open(c, size));
        let codec = result.as_ref().ok().map(|o| o.codec);
        if needs_h264_size(Codec::Av1, codec) {
            limit = H264_MAX_SIDE;
        }
        if let Ok(o) = result {
            opened = Some((o.codec, o.value));
            break;
        }
    }
    assert_eq!(opened, Some((Codec::H264, (4096, 1152))), "the first open fails at 5120 wide, the next fits");
    assert!(!needs_h264_size(Codec::H264, Some(Codec::H264)), "H.264 wanted: its limit was already in force");
    assert!(!needs_h264_size(Codec::Hevc, Some(Codec::Hevc)), "HEVC opened: 8192 stays");
}
