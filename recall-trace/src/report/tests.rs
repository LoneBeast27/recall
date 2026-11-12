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
    Names { user: Some("someone".into()), pc: Some("DESKTOP-ABC123".into()) }
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
fn the_vocabulary_holds_recalls_words_and_no_game_names() {
    for word in ["foreground", "pid", "record", "notice", "island", "storage", "favourite", "save"] {
        assert!(redact::known_word(word), "{word}");
    }
    for word in ["minecraft", "valorant", "r5apex", "javaw", "discord", "steam", "chatgpt"] {
        assert!(!redact::known_word(word), "{word}");
    }
}

/// The plain words build.rs keeps although the tables hold them (its `PLAIN` list).
const PLAIN_TABLE_WORDS: &[&str] = &[
    "listed", "unknown", "other", "unreadable", "class", "game", "signal", "line", "recall", "none", "code", "store", "mkv", "moov", "uuid", "meta", "vide", "soun", "hdlr",
    "trak", "ftyp", "mdat", "recording", "output_dir", "managed", "rec", "game_folder_choices", "appdata", "detect", "exe", "fullscreen", "fullscreen_games", "games",
    "kind", "local", "never", "overrides", "path", "programs", "record", "remove", "unlist", "users",
];

/// Lower-case one-word literals in a source file (`"msedge"`, `"obs64"`): table entries.
fn table_words(file: &str) -> Vec<String> {
    let text = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(file)).unwrap();
    text.split('"')
        .skip(1)
        .step_by(2)
        .filter(|w| w.len() >= 3 && !w.starts_with('.') && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.')))
        .map(|w| w.trim_end_matches(".exe").to_string())
        .collect()
}

#[test]
fn no_name_from_recalls_app_and_game_tables_is_a_vocabulary_word() {
    let tables = [
        "../recall-hdr/src/known_games.rs",
        "../recall-hdr/src/private_app_stems.rs",
        "../recall-hdr/src/record_target_rules.rs",
        "../recall-hdr/src/rules.rs",
        "../recall-native/src/services/library/edit_marks.rs",
        "../recall-native/src/views/folders_settings.rs",
    ];
    let mut leaked = Vec::new();
    for table in tables {
        for word in table_words(table) {
            let pieces: Vec<&str> = word.split(['_', '-', '.']).filter(|p| !p.is_empty()).collect();
            let plain = PLAIN_TABLE_WORDS.contains(&word.as_str());
            if !plain && (redact::known_word(&word) || (pieces.len() == 1 && redact::known_word(pieces[0]))) {
                leaked.push(format!("{table}: {word}"));
            }
        }
    }
    assert!(leaked.is_empty(), "{leaked:#?}");
    for name in ["msedge", "opera_gx", "teamviewer", "riotclientservices", "obs64", "netflix", "premiere", "davinci", "capcut", "medal", "shadowplay"] {
        assert!(!redact::known_word(name), "{name}");
    }
}

#[test]
fn game_name_slots_are_always_replaced() {
    let log = "[1] record: target -> \"THE FINALS\" (x.exe, pid 4)\n[2] record: notice game: Recording Ready or Not\n[recording.game_folder_choices]\nthe_finals = \"own\"\n[recording]\nfps = 60\n";
    let out = redact::redact(log, &names());
    for gone in ["FINALS", "Ready", "Not", "the_finals"] {
        assert!(!out.contains(gone), "{gone} left in:\n{out}");
    }
    assert!(out.contains("target -> \"<app>\""), "{out}");
    assert!(out.contains("notice game: Recording <app>\n"), "{out}");
    assert!(out.contains("<app> = \"own\""), "{out}");
    assert!(out.contains("fps = 60"), "{out}");
}

#[test]
fn units_and_percentages_stay_readable() {
    let out = redact::redact("record: saved in 812ms at 2560x1440, 60fps, 45% load, CPU 30%\n", &names());
    assert!(out.contains("812ms at 2560x1440, 60fps, 45% load"), "{out}");
    assert!(!out.contains("<path>"), "{out}");
}

#[test]
fn logs_keep_only_recalls_own_words() {
    let log = concat!(
        "[1794326113091] === recall-hdr start (log: C:\\Users\\someone\\AppData\\Roaming\\recall-hdr\\recall-hdr.log) ===\n",
        "[1794326115092] foreground: Some Client.exe pid 33636 class VALORANTUnrealWindow rect [2560,-1115,4000,1373]\n",
        "[3] record: notice game: Recording VALORANT Apex Legends\n",
        "[4] storage: Funny Moment 2026-11-12.mp4 became a favourite\n",
        "[5] focused rule set: r5apex -> Game\n",
        "[6] gpu games: C:\\Program Files (x86)\\Steam\\steamapps\\common\\Apex Legends\\r5apex.exe (dx12)\n",
        "[7] game title 'Minecraft 1.21.1' on DESKTOP-ABC123 by Someone (mail a.b@example.com)\n",
        "[8] audio: endpoint {0.0.1.00000000}.{3f2a9c1e-5b7d-4e8a-9c21-7d4e5f6a8b90} Headset (Arctis Nova 7)\n",
        "[9] record-hdr: managed display 'LG ULTRAGEAR' found\n",
        "{\"unix_ms\":1,\"event\":\"audio underrun\",\"detail\":{\"clip\": \"Funny \\\"moment\\\".mp4\",\"new\":3}}\n",
        "[recording.game_folder_choices]\nVALORANT = \"merge\"\n\"Apex Legends\" = \"own\"\n",
        "out_dir = \"%USERPROFILE%\\Videos\\Recall\"\n",
    );
    let out = redact::redact(log, &names());
    let gone = [
        "someone", "Someone", "Client", "VALORANT", "Apex", "Legends", "Funny", "Moment", "r5apex", "Steam", "steamapps", "Minecraft", "DESKTOP", "ABC123", "example", "3f2a9c1e", "Arctis",
        "Nova", "Headset", "ULTRAGEAR", "Videos", "Program", "x86", "moment",
    ];
    for gone in gone {
        assert!(!out.contains(gone), "{gone} left in:\n{out}");
    }
    let kept = ["foreground: <app> pid 33636", "rect [2560,-1115,4000,1373]", "(<w>)\n","\"new\":3", "unix_ms", "<path>\n", "storage: <file>.mp4 became a favourite", "<app> = \"merge\"\n", "<app> = \"own\""];
    for kept in kept {
        assert!(out.contains(kept), "{kept} missing:\n{out}");
    }
    assert!(out.contains("on <pc> by <user>"), "{out}");
}

#[test]
fn emails_are_replaced() {
    assert_eq!(redact::redact("record: sent to a.b@example.com\n", &names()), "record: sent to <email>\n");
}

#[test]
fn a_short_user_name_replaces_only_whole_words() {
    let names = Names { user: Some("al".into()), pc: None };
    let out = redact::redact("record: al saved all clips\n", &names);
    assert!(out.starts_with("record: <user> saved all"), "{out}");
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
    assert_eq!(clean, vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, b'J', b'F', 0xFF, 0xDA, 0, 2, 9, 0xFF, 0x00, 9, 0xFF, 0xD9]);
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
    // moov inside moov, 40 deep: refused, not a stack overflow.
    let mut nested = mp4_box(b"free", b"");
    for _ in 0..40 {
        nested = mp4_box(b"moov", &nested);
    }
    let file = [mp4_box(b"ftyp", b"isom\0\0\0\0isom"), nested].concat();
    assert_eq!(media::clean(Kind::Mp4, file), None);
}

#[test]
fn a_report_holds_the_text_traces_redacted_logs_and_renamed_media() {
    let dir = temp_dir("build");
    let trace = dir.join("trace");
    fs::create_dir_all(&trace).unwrap();
    fs::write(trace.join("island.trace"), "{\"t\":0,\"e\":\"island.state\",\"f\":{\"game\":\"minecraft_java\"}}\n").unwrap();
    fs::write(trace.join("crash-island-1.json"), "{\"kind\":\"panic\"}").unwrap();
    fs::write(trace.join("notes.txt"), "not ours").unwrap();
    let log = dir.join("recall-hdr.log");
    fs::write(&log, "start C:\\Users\\someone\\x.log\n").unwrap();
    let shot = dir.join("My secret name.png");
    fs::write(&shot, png_with_metadata()).unwrap();
    let input = Input {
        text: "Island missing in Minecraft\nIt never shows.".into(),
        version: "0.1.5".into(),
        windows: "26300".into(),
        trace_dir: Some(trace),
        logs: vec![("recall-hdr.log".into(), log)],
        media: vec![shot],
    };
    let prepared = prepare(&input, &names()).unwrap();
    let listed: Vec<String> = prepared.files().into_iter().map(|(n, _)| n).collect();
    assert_eq!(listed, ["report.txt", "trace/crash-island-1.json", "trace/island.trace", "logs/recall-hdr.log", "media/media-1.png"]);
    let path = save(&prepared, &dir.join("reports")).unwrap();
    assert!(path.ends_with("report-1.zip"));
    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes.len() as u64, prepared.zip_bytes(), "the size shown is the size written");
    let entries = unzip(&bytes);
    let text = |name: &str| String::from_utf8(entries.iter().find(|(n, _)| n == name).unwrap().1.clone()).unwrap();
    assert!(text("report.txt").starts_with("Recall 0.1.5\nWindows 26300\n\nIsland missing in Minecraft"));
    assert_eq!(text("logs/recall-hdr.log"), "start <path>\n");
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
