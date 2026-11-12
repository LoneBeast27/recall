//! Report tests: the zip opens, logs keep only Recall's own words, only allowed media go in
//! with their metadata gone, and the caps hold. Every file goes to a fresh temp folder.

use std::fs;
use std::path::PathBuf;

use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-report-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn names() -> Names {
    Names { user: Some("someone".into()) }
}

/// Read back a stored zip: (name, data) per entry, checking every CRC.
fn unzip(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let le16 = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
    let le32 = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let end = bytes.len() - 22;
    assert_eq!(le32(end), 0x0605_4B50);
    let (count, mut cd) = (le16(end + 10), le32(end + 16));
    let mut out = Vec::new();
    for _ in 0..count {
        assert_eq!(le32(cd), 0x0201_4B50);
        let (crc, size, name_len, local) = (le32(cd + 16) as u32, le32(cd + 20), le16(cd + 28), le32(cd + 42));
        let name = String::from_utf8(bytes[cd + 46..cd + 46 + name_len].to_vec()).unwrap();
        let data_at = local + 30 + le16(local + 26);
        let data = bytes[data_at..data_at + size].to_vec();
        assert_eq!(zip::crc32(&data), crc, "{name}");
        out.push((name, data));
        cd += 46 + name_len;
    }
    out
}

#[test]
fn crc32_matches_the_standard_check_value() {
    assert_eq!(zip::crc32(b"123456789"), 0xCBF4_3926);
}

#[test]
fn the_settings_summary_holds_only_allowed_keys_and_counts() {
    let config = concat!(
        "version = 3\n[display]\ntarget_friendly_name = \"LG ULTRAGEAR\"\n",
        "[recording]\nauto_record = true\noutput_dir = \"C:\\\\Users\\\\someone\\\\Videos\"\nfps = 60\ncodec = \"hevc\"\nquality = \"Ready or Not\"\n",
        "voice_app = \"discord\"\nmic_device = \"Headset (Arctis Nova 7)\"\napp_audio = [\"spotify\", \"discord\"]\nsave_hotkey = \"Alt+N\"\n",
        "[recording.game_folder_choices]\nVALORANT = \"merge\"\nthe_finals = \"own\"\n",
        "[[recording.audio_sources]]\nid = \"mic\"\ndevice = \"Headset\"\n[[recording.audio_sources]]\nid = \"game\"\n",
        "[overlay]\ncorner = \"top-left\"\nstats = maybe\n",
        "[detect]\nfullscreen_games = [\"control\", \"Ready or Not\"]\n",
        "[[overrides]]\nexe = \"r5apex\"\nkind = \"game\"\n",
    );
    let out = settings::summary(config);
    for gone in ["LG", "ULTRAGEAR", "someone", "Videos", "Ready", "discord", "spotify", "Headset", "Arctis", "Alt", "VALORANT", "finals", "control", "r5apex", "mic\""] {
        assert!(!out.contains(gone), "{gone} left in:\n{out}");
    }
    let kept = [
        "version = 3", "recording.auto_record = true", "recording.fps = 60", "recording.codec = hevc", "recording.quality = ?", "overlay.corner = top-left", "overlay.stats = ?",
        "recording.audio_sources: 2 entries", "recording.game_folder_choices: 2 entries", "detect.fullscreen_games: 2 entries", "recording.app_audio: 2 entries", "overrides: 1 entries",
    ];
    for kept in kept {
        assert!(out.contains(kept), "{kept} missing:\n{out}");
    }
}

#[test]
fn only_pictures_and_mp4_clips_are_taken_by_their_first_bytes() {
    assert_eq!(media::sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]), Some(Kind::Png));
    assert_eq!(media::sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Kind::Jpeg));
    assert_eq!(media::sniff(b"\0\0\0\x18ftypisom"), Some(Kind::Mp4));
    assert_eq!(media::sniff(b"\0\0\0\x18ftypmp42"), Some(Kind::Mp4));
    for refused in [&b"\0\0\0\x14ftypqt  "[..], b"\0\0\0\x18ftypheic", b"\0\0\0\x1cftypavif", b"\0\0\0\x18ftyp3gp4", b"\0\0\0\x20ftypM4A ", b"\0\0\0\x18ftypcrx ", b"PK\x03\x04 a zip", b"MZ\x90\0"] {
        assert_eq!(media::sniff(refused), None, "{:?}", String::from_utf8_lossy(refused));
    }
    assert!(media::refusal(10, None).is_some());
    assert!(media::refusal(MAX_FILE + 1, Some(Kind::Mp4)).is_some());
    assert!(media::refusal(MAX_FILE, Some(Kind::Mp4)).is_none());
}

fn png_chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
    let mut c = (data.len() as u32).to_be_bytes().to_vec();
    c.extend(kind);
    c.extend(data);
    let mut crc_in = kind.to_vec();
    crc_in.extend(data);
    c.extend(zip::crc32(&crc_in).to_be_bytes());
    c
}

fn png_with_metadata() -> Vec<u8> {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend(png_chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]));
    png.extend(png_chunk(b"iCCP", b"profile\0\0x"));
    png.extend(png_chunk(b"tEXt", b"Author\0someone"));
    png.extend(png_chunk(b"caBX", b"c2pa manifest"));
    png.extend(png_chunk(b"IDAT", &[1, 2, 3]));
    png.extend(png_chunk(b"IEND", &[]));
    png
}

#[test]
fn png_keeps_only_its_picture_chunks() {
    let png = media::clean(Kind::Png, png_with_metadata()).unwrap();
    for gone in [&b"tEXt"[..], b"iCCP", b"caBX", b"someone"] {
        assert!(!png.windows(gone.len()).any(|w| w == gone), "{}", String::from_utf8_lossy(gone));
    }
    assert!(png.windows(4).any(|w| w == b"IDAT"));
    assert_eq!(media::clean(Kind::Png, b"\x89PNG\r\n\x1a\nbroken".to_vec()), None);
}

#[test]
fn jpeg_loses_its_metadata_and_whatever_follows_the_image() {
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, b'J', b'F'];
    jpeg.extend([0xFF, 0xE1, 0, 8, b'E', b'x', b'i', b'f', 0, 0]);
    jpeg.extend([0xFF, 0xE2, 0, 6, b'M', b'P', b'F', 0]);
    jpeg.extend([0xFF, 0xFE, 0, 6, b'h', b'i', b'!', b'!']);
    jpeg.extend([0xFF, 0xDA, 0, 2, 9, 0xFF, 0x00, 9, 0xFF, 0xD9]);
    jpeg.extend(b"\0\0\0\x18ftypmp42 motion photo video");
    let clean = media::clean(Kind::Jpeg, jpeg).unwrap();
    assert_eq!(clean, vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDA, 0, 2, 9, 0xFF, 0x00, 9, 0xFF, 0xD9]);
}

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    b.extend(kind);
    b.extend(body);
    b
}

/// A small MP4 with a place, a tool name, a handler name and creation times in it.
fn mp4_with_metadata() -> Vec<u8> {
    let full = |times: [u8; 8], rest: &[u8]| {
        let mut b = vec![0, 0, 0, 0];
        b.extend(times);
        b.extend(rest);
        b
    };
    let mut hdlr = vec![0u8; 24];
    hdlr[8..12].copy_from_slice(b"vide");
    hdlr.extend(b"Someone's phone\0");
    let mdia = [mp4_box(b"mdhd", &full([1, 2, 3, 4, 5, 6, 7, 8], &[0; 12])), mp4_box(b"hdlr", &hdlr)].concat();
    let trak = [mp4_box(b"tkhd", &full([1, 2, 3, 4, 5, 6, 7, 8], &[0; 72])), mp4_box(b"mdia", &mdia)].concat();
    let udta = mp4_box(b"udta", &mp4_box(b"\xa9xyz", b"+48.8584+002.2945/"));
    let moov = [mp4_box(b"mvhd", &full([1, 2, 3, 4, 5, 6, 7, 8], &[0; 88])), mp4_box(b"trak", &trak), udta].concat();
    [mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso2avc1mp41"), mp4_box(b"moov", &moov), mp4_box(b"uuid", b"vendor data here"), mp4_box(b"mdat", b"frames")].concat()
}

#[test]
fn mp4_metadata_is_blanked_in_place_and_times_zeroed() {
    let original = mp4_with_metadata();
    let clean = media::clean(Kind::Mp4, original.clone()).unwrap();
    assert_eq!(clean.len(), original.len(), "same size: sample offsets stay right");
    for gone in [&b"+48.8584"[..], b"Someone", b"vendor data", b"udta", b"uuid", b"\x01\x02\x03\x04\x05\x06\x07\x08"] {
        assert!(!clean.windows(gone.len()).any(|w| w == gone), "{}", String::from_utf8_lossy(gone));
    }
    for kept in [&b"ftypisom"[..], b"mdatframes", b"mvhd", b"tkhd", b"mdhd", b"hdlr"] {
        assert!(clean.windows(kept.len()).any(|w| w == kept), "{}", String::from_utf8_lossy(kept));
    }
    assert_eq!(media::clean(Kind::Mp4, b"\0\0\0\x40ftypisom".to_vec()), None, "a box past the end");
}

#[test]
fn mp4_with_a_data_track_or_deep_nesting_is_refused() {
    // A GPS track (GoPro gpmd, DJI camm): its handler is "meta", not video or sound.
    let mut hdlr = vec![0u8; 24];
    hdlr[8..12].copy_from_slice(b"meta");
    let trak = mp4_box(b"trak", &mp4_box(b"mdia", &mp4_box(b"hdlr", &hdlr)));
    let file = [mp4_box(b"ftyp", b"mp41\0\0\0\0mp41"), mp4_box(b"moov", &trak), mp4_box(b"mdat", b"gps samples")].concat();
    assert_eq!(media::clean(Kind::Mp4, file), None);
    // moov inside moov, 40 deep: an inner moov is not an allowed child, so it is blanked,
    // not walked (no deep recursion); only the outer moov is left.
    let mut nested = mp4_box(b"free", b"");
    for _ in 0..40 {
        nested = mp4_box(b"moov", &nested);
    }
    let file = [mp4_box(b"ftyp", b"isom\0\0\0\0isom"), nested].concat();
    let clean = media::clean(Kind::Mp4, file).unwrap();
    assert_eq!(clean.windows(4).filter(|w| w == b"moov").count(), 1);
}

#[test]
fn mp4_keeps_only_allowed_boxes_and_needs_a_handler_per_track() {
    // An unknown child of moov (a place box) and a top-level free box with bytes in it.
    let mut hdlr = vec![0u8; 24];
    hdlr[8..12].copy_from_slice(b"soun");
    let trak = mp4_box(b"trak", &mp4_box(b"mdia", &mp4_box(b"hdlr", &hdlr)));
    let moov = [mp4_box(b"\xa9xyz", b"+48.8584+002.2945/"), trak].concat();
    let file = [mp4_box(b"ftyp", b"isom\0\0\0\0isom"), mp4_box(b"moov", &moov), mp4_box(b"free", b"leftover name"), mp4_box(b"mdat", b"x")].concat();
    let clean = media::clean(Kind::Mp4, file).unwrap();
    for gone in [&b"+48.8584"[..], b"leftover", b"\xa9xyz"] {
        assert!(!clean.windows(gone.len()).any(|w| w == gone), "{}", String::from_utf8_lossy(gone));
    }
    // A track with no handler at all.
    let trak = mp4_box(b"trak", &mp4_box(b"tkhd", &[0; 84]));
    let file = [mp4_box(b"ftyp", b"isom\0\0\0\0isom"), mp4_box(b"moov", &trak), mp4_box(b"mdat", b"gps")].concat();
    assert_eq!(media::clean(Kind::Mp4, file), None);
}

/// A file with one video track whose sample description holds `entry`, and a `dref` holding
/// `dref_entry`.
fn mp4_with_sample_entry(entry: &[u8], dref_entry: &[u8]) -> Vec<u8> {
    let mut hdlr = vec![0u8; 24];
    hdlr[8..12].copy_from_slice(b"vide");
    let stsd = mp4_box(b"stsd", &[&[0, 0, 0, 0, 0, 0, 0, 1][..], entry].concat());
    let dref = mp4_box(b"dref", &[&[0, 0, 0, 0, 0, 0, 0, 1][..], dref_entry].concat());
    let minf = [mp4_box(b"dinf", &dref), mp4_box(b"stbl", &stsd)].concat();
    let mdia = [mp4_box(b"hdlr", &hdlr), mp4_box(b"minf", &minf)].concat();
    let moov = mp4_box(b"moov", &mp4_box(b"trak", &mp4_box(b"mdia", &mdia)));
    [mp4_box(b"ftyp", b"isom\0\0\0\0isom"), moov, mp4_box(b"mdat", b"frames")].concat()
}

fn self_contained_url() -> Vec<u8> {
    mp4_box(b"url ", &[0, 0, 0, 1])
}

#[test]
fn sample_descriptions_lose_encoder_names_profiles_and_vendor_boxes() {
    // avc1: 78-byte fixed part with an encoder name, then avcC, a vendor uuid with a serial
    // and a path, udta, and a colour profile.
    let mut fixed = vec![0u8; 78];
    fixed[42..59].copy_from_slice(b"GoPro AVC encoder");
    let mut colr_prof = b"prof".to_vec();
    colr_prof.extend(b"ICC maker someone 2026-11-12");
    let avc1 = mp4_box(
        b"avc1",
        &[
            fixed,
            mp4_box(b"avcC", b"\x01\x64\x00\x28"),
            mp4_box(b"uuid", b"serial C3441325 C:\\Users\\someone DESKTOP-ABC"),
            mp4_box(b"udta", b"someone"),
            mp4_box(b"colr", &colr_prof),
            mp4_box(b"colr", b"nclx\x00\x01\x00\x01\x00\x01\x00"),
            mp4_box(b"pasp", &[0, 0, 0, 1, 0, 0, 0, 1]),
        ]
        .concat(),
    );
    let clean = media::clean(Kind::Mp4, mp4_with_sample_entry(&avc1, &self_contained_url())).unwrap();
    for gone in [&b"GoPro"[..], b"C3441325", b"someone", b"DESKTOP", b"ICC maker", b"prof"] {
        assert!(!clean.windows(gone.len()).any(|w| w == gone), "{}", String::from_utf8_lossy(gone));
    }
    for kept in [&b"avc1"[..], b"avcC", b"nclx", b"pasp", b"mdatframes"] {
        assert!(clean.windows(kept.len()).any(|w| w == kept), "{}", String::from_utf8_lossy(kept));
    }
    // mp4a with a meta child.
    let mp4a = mp4_box(b"mp4a", &[vec![0u8; 28], mp4_box(b"esds", b"\x00\x00\x00\x00es"), mp4_box(b"meta", b"someone's phone")].concat());
    let clean = media::clean(Kind::Mp4, mp4_with_sample_entry(&mp4a, &self_contained_url())).unwrap();
    assert!(!clean.windows(7).any(|w| w == b"someone"));
    assert!(clean.windows(4).any(|w| w == b"esds"));
}

#[test]
fn an_unknown_coding_or_a_reference_to_another_file_is_refused() {
    let mut fixed = vec![0u8; 78];
    fixed[42..46].copy_from_slice(b"name");
    let gpmd = mp4_box(b"gpmd", &fixed);
    assert_eq!(media::clean(Kind::Mp4, mp4_with_sample_entry(&gpmd, &self_contained_url())), None);
    let avc1 = mp4_box(b"avc1", &[vec![0u8; 78], mp4_box(b"avcC", b"\x01")].concat());
    let mut url = vec![0, 0, 0, 0];
    url.extend(b"file:///C:/Users/someone/Videos/Game/clip.mp4\0");
    assert_eq!(media::clean(Kind::Mp4, mp4_with_sample_entry(&avc1, &mp4_box(b"url ", &url))), None);
    // A quicktime sound entry (version 1 has more fields) is refused rather than misread.
    let mut qt = vec![0u8; 28];
    qt[9] = 1;
    assert_eq!(media::clean(Kind::Mp4, mp4_with_sample_entry(&mp4_box(b"mp4a", &qt), &self_contained_url())), None);
}

#[test]
fn a_progressive_jpeg_loses_metadata_between_its_scans() {
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, b'J', b'F'];
    jpeg.extend([0xFF, 0xDA, 0, 2, 1, 2, 0xFF, 0x00, 3]);
    jpeg.extend([0xFF, 0xE1, 0, 8, b'E', b'x', b'i', b'f', 0, 0]);
    jpeg.extend([0xFF, 0xC4, 0, 3, 7]);
    jpeg.extend([0xFF, 0xDA, 0, 2, 4, 0xFF, 0xD0, 5, 0xFF, 0xD9]);
    jpeg.extend(b"trailing");
    let clean = media::clean(Kind::Jpeg, jpeg).unwrap();
    let expect = [vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDA, 0, 2, 1, 2, 0xFF, 0x00, 3, 0xFF, 0xC4, 0, 3, 7, 0xFF, 0xDA, 0, 2, 4, 0xFF, 0xD0, 5, 0xFF, 0xD9]].concat();
    assert_eq!(clean, expect);
}

#[test]
fn jpeg_markers_after_fill_bytes_are_read_and_odd_segments_dropped() {
    // Fill bytes before a marker, an oversized APP14, a JPGn (F5) segment and a thumbnail in APP0.
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xFF, 0xE0, 0, 8, b'J', b'F', b'I', b'F', 0, 9];
    jpeg.extend([0xFF, 0xEE, 0, 20]);
    jpeg.extend(b"Adobe and more da");
    jpeg.push(0);
    jpeg.extend([0xFF, 0xF5, 0, 6, b'n', b'a', b'm', b'e']);
    jpeg.extend([0xFF, 0xEE, 0, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, 1]);
    jpeg.extend([0xFF, 0xFF, 0xFF, 0xDA, 0, 2, 7, 0xFF, 0xD9]);
    let clean = media::clean(Kind::Jpeg, jpeg).unwrap();
    for gone in [&b"more da"[..], b"name"] {
        assert!(!clean.windows(gone.len()).any(|w| w == gone), "{}", String::from_utf8_lossy(gone));
    }
    let adobe = [0xFF, 0xEE, 0, 14, b'A', b'd', b'o', b'b', b'e'];
    assert!(clean.windows(adobe.len()).any(|w| w == adobe), "the 14-byte APP14 stays");
    assert!(clean.ends_with(&[0xFF, 0xDA, 0, 2, 7, 0xFF, 0xD9]));
}

#[test]
fn every_track_needs_its_own_handler_and_short_entries_are_refused() {
    let mut hdlr = vec![0u8; 24];
    hdlr[8..12].copy_from_slice(b"vide");
    let with = mp4_box(b"trak", &mp4_box(b"mdia", &mp4_box(b"hdlr", &hdlr)));
    let both = mp4_box(b"trak", &mp4_box(b"mdia", &[mp4_box(b"hdlr", &hdlr), mp4_box(b"hdlr", &hdlr)].concat()));
    let without = mp4_box(b"trak", &mp4_box(b"tkhd", &[0; 84]));
    // One track with two handlers and one with none: the file-wide count matches, the file is
    // still refused.
    let file = [mp4_box(b"ftyp", b"isom\0\0\0\0isom"), mp4_box(b"moov", &[both, without].concat()), mp4_box(b"mdat", b"x")].concat();
    assert_eq!(media::clean(Kind::Mp4, file), None);
    let ok = [mp4_box(b"ftyp", b"isom\0\0\0\0isom"), mp4_box(b"moov", &with), mp4_box(b"mdat", b"x"), vec![1, 2, 3]].concat();
    let clean = media::clean(Kind::Mp4, ok).unwrap();
    assert!(clean.ends_with(&[0, 0, 0]), "trailing bytes too few to be a box are zeroed");
    // An avc1 entry shorter than its fixed part.
    let short = mp4_box(b"avc1", &[0u8; 40]);
    assert_eq!(media::clean(Kind::Mp4, mp4_with_sample_entry(&short, &self_contained_url())), None);
}

#[test]
fn settings_lists_are_counted_however_they_are_written() {
    let config = concat!(
        "[detect] # games that must be fullscreen\n",
        "fullscreen_games = [\n  \"control\",\n  \"Ready, or Not\", # a comma in a name\n]\n",
        "[recording]\napp_audio = [\"a\", \"b\", \"c\"]\nfps = 60 # comment\n",
    );
    let out = settings::summary(config);
    assert!(out.contains("detect.fullscreen_games: 2 entries"), "{out}");
    assert!(out.contains("recording.app_audio: 3 entries"), "{out}");
    assert!(out.contains("recording.fps = 60\n"), "{out}");
    assert!(!out.contains("control") && !out.contains("Ready"), "{out}");
}

#[test]
fn files_over_their_cap_are_not_read() {
    let dir = temp_dir("capped");
    let trace = dir.join("trace");
    fs::create_dir_all(&trace).unwrap();
    let big = vec![b'x'; 5 * 1024 * 1024];
    fs::write(trace.join("island.trace"), &big).unwrap();
    fs::write(dir.join("config.toml"), &big).unwrap();
    let input = Input { text: "x".into(), trace_dir: Some(trace), settings: Some(dir.join("config.toml")), ..Input::default() };
    let files: Vec<String> = prepare(&input, &names()).unwrap().files().into_iter().map(|(n, _)| n).collect();
    assert_eq!(files, ["report.txt"]);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_short_handler_cannot_borrow_the_next_boxes_bytes() {
    // An 8-byte hdlr (no body) followed by a box whose bytes would read as "vide".
    // The hdlr's handler field would sit 8 bytes past its header: the next box's body.
    let mdia = [mp4_box(b"hdlr", b""), mp4_box(b"free", b"vide\0\0\0\0")].concat();
    let trak = mp4_box(b"trak", &mp4_box(b"mdia", &mdia));
    let file = [mp4_box(b"ftyp", b"isom\0\0\0\0isom"), mp4_box(b"moov", &trak), mp4_box(b"mdat", b"gps")].concat();
    assert_eq!(media::clean(Kind::Mp4, file), None);
}

#[test]
fn an_unclosed_list_in_a_large_settings_file_is_read_in_one_pass() {
    let mut config = String::from("[detect]\nfullscreen_games = [\n");
    for i in 0..200_000 {
        config.push_str(&format!("  \"game{i}\",\n"));
    }
    let started = std::time::Instant::now();
    let out = settings::summary(&config);
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
    assert!(!out.contains("game1"), "{out}");
}

#[test]
fn a_report_holds_the_text_traces_a_settings_summary_and_renamed_media() {
    let dir = temp_dir("build");
    let trace = dir.join("trace");
    fs::create_dir_all(&trace).unwrap();
    fs::write(trace.join("island.trace"), "{\"t\":0,\"e\":\"island.state\",\"f\":{\"game\":\"minecraft_java\"}}\n").unwrap();
    fs::write(trace.join("crash-island-1.json"), "{\"kind\":\"panic\"}").unwrap();
    fs::write(trace.join("notes.txt"), "not ours").unwrap();
    let config = dir.join("config.toml");
    fs::write(&config, "[recording]\noutput_dir = 'C:\\Users\\someone\\Videos'\nfps = 60\n").unwrap();
    let shot = dir.join("My secret name.png");
    fs::write(&shot, png_with_metadata()).unwrap();
    let input = Input {
        text: "Island missing in Minecraft\nIt never shows.".into(),
        version: "0.1.5".into(),
        windows: "26300".into(),
        trace_dir: Some(trace),
        settings: Some(config),
        media: vec![shot],
    };
    let prepared = prepare(&input, &names()).unwrap();
    let listed: Vec<String> = prepared.files().into_iter().map(|(n, _)| n).collect();
    assert_eq!(listed, ["report.txt", "trace/crash-island-1.json", "trace/island.trace", "settings.txt", "media/media-1.png"]);
    let path = save(&prepared, &dir.join("reports")).unwrap();
    assert!(path.ends_with("report-1.zip"));
    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes.len() as u64, prepared.zip_bytes(), "the size shown is the size written");
    let entries = unzip(&bytes);
    let text = |name: &str| String::from_utf8(entries.iter().find(|(n, _)| n == name).unwrap().1.clone()).unwrap();
    assert!(text("report.txt").starts_with("Recall 0.1.5\nWindows 26300\n\nIsland missing in Minecraft"));
    assert!(text("settings.txt").starts_with("recording.fps = 60\n"), "{}", text("settings.txt"));
    assert!(!text("settings.txt").contains("someone") && !text("settings.txt").contains("Videos"));
    assert!(save(&prepared, &dir.join("reports")).unwrap().ends_with("report-2.zip"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_caps_hold_before_anything_is_read() {
    let dir = temp_dir("cap");
    let mut media = Vec::new();
    for i in 0..2 {
        let clip = dir.join(format!("clip{i}.mp4"));
        let mut data = b"\0\0\0\x18ftypisom".to_vec();
        data.resize(MAX_FILE as usize, 0);
        fs::write(&clip, data).unwrap();
        media.push(clip);
    }
    let input = Input { media: media.clone(), ..Input::default() };
    assert!(prepare(&input, &names()).err().unwrap().contains("over 24 MB"));
    let seven = Input { media: vec![media[0].clone(); MAX_MEDIA + 1], ..Input::default() };
    assert!(prepare(&seven, &names()).err().unwrap().contains("up to 6 files"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_issue_address_carries_only_the_text_and_summary_and_fits() {
    let input = Input { text: "Overlay & stats gone?\nSince the update.".into(), version: "0.1.5".into(), windows: "26300".into(), ..Input::default() };
    let url = issue_url(&input);
    assert!(url.starts_with("https://github.com/LoneBeast27/recall/issues/new?title=Overlay%20%26%20stats%20gone%3F&body=Since%20the%20update."), "{url}");
    assert!(url.contains("Recall%200.1.5%0AWindows%2026300"), "{url}");
    assert!(!url["https://github.com/LoneBeast27/recall/issues/new?title=Overlay".len()..].contains("Overlay"), "the title is not repeated");
    let long = Input { text: format!("Title\n{}", "日本語のテキスト ".repeat(800)), ..Input::default() };
    assert!(issue_url(&long).len() <= 7000);
}

#[test]
fn reports_are_off_in_test_runs() {
    let vars = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
    assert_eq!(reports_dir_from(vars(&[("LOCALAPPDATA", r"C:\L")])), Some(PathBuf::from(r"C:\L").join("Recall").join("reports")));
    assert_eq!(reports_dir_from(vars(&[("LOCALAPPDATA", r"C:\L"), ("RECALL_NATIVE_OFFSCREEN", "1")])), None);
    assert_eq!(reports_dir_from(vars(&[("RECALL_NATIVE_OFFSCREEN", "1"), ("RECALL_REPORTS_DIR", r"D:\r")])), Some(PathBuf::from(r"D:\r")));
}

#[test]
fn clearing_the_trace_removes_only_its_own_plain_files() {
    let dir = temp_dir("clear");
    for name in ["island.trace", "island.trace.1", "crash-probe-1.json", "crash-probe-native.pending", "keep.mp4", "config.toml", "crash-1.mp4", "a.trace.mp4"] {
        fs::write(dir.join(name), "x").unwrap();
    }
    // A junction named like a trace, pointing at a folder with a clip in it.
    let elsewhere = temp_dir("clear-elsewhere");
    fs::write(elsewhere.join("clip.mp4"), "clip").unwrap();
    let junction = dir.join("x.trace");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&elsewhere)
        .output()
        .is_ok_and(|o| o.status.success());
    assert_eq!(clear_trace(&dir), 4);
    for kept in ["keep.mp4", "config.toml", "crash-1.mp4", "a.trace.mp4"] {
        assert!(dir.join(kept).exists(), "{kept}");
    }
    assert!(elsewhere.join("clip.mp4").exists());
    assert!(made, "the junction was made, so the check below is real");
    assert!(junction.exists(), "the junction itself stays");
    if made {
        let _ = fs::remove_dir(&junction); // removes the link only
    }
    let _ = fs::remove_dir_all(dir);
    let _ = fs::remove_dir_all(elsewhere);
}

#[test]
fn game_tags_pass_the_content_check_but_game_names_do_not() {
    assert_eq!(crate::find_content(r#"{"game":"minecraft_java"}"#, None), None);
    assert!(crate::find_content(r#"{"game":"minecraft_java_modded"}"#, None).is_some());
    assert!(crate::find_content(r#"{"why":"minecraft"}"#, None).is_some());
}
