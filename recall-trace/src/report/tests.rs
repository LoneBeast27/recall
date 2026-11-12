//! Report tests: the zip opens, logs lose every name and path, only allowed media go in, and
//! the size cap holds. Every file goes to a fresh temp folder.

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
fn logs_lose_names_paths_programs_and_clips() {
    let log = concat!(
        "[1794326113091] === recall-hdr start (log: C:\\Users\\someone\\AppData\\Roaming\\recall-hdr\\recall-hdr.log) ===\n",
        "[1794326115092] foreground: ChatGPT.exe pid 33636 class Chrome_WidgetWin_1 rect [2560,-1115,4000,1373]\n",
        "save to D:\\My Clips\\Minecraft 2026.mp4, done\n",
        "game title 'Minecraft 1.21.1' on DESKTOP-ABC123 by Someone (mail a.b@example.com)\n",
        "{\"unix_ms\":1,\"event\":\"audio underrun\",\"detail\":{\"clip\":\"Funny moment.mp4\",\"new\":3}}\n",
        "out_dir = \"%USERPROFILE%\\Videos\\Recall\"\n",
        "[1] raise: re-raised over Game.exe pid 7 class VALORANTUnrealWindow rect [0,0,2560,1440] topmost yes\n",
        "[2] placed: discord anchor TopCentre (placed by user) monitor [0,0,2560,1440]\n",
        "[3] editor: on for valorantwin64shipping\n",
        "[4] editor: saved Right for cs2 (previous held 3 more calls)\n",
        "[5] editor: Leave: nothing more written for discord\n",
        "[6] foreground: Spaced Client.exe pid 9 class X rect [0,0,1,1]\n",
    );
    let out = redact::redact(log, &names());
    let gone = ["someone", "Someone", "ChatGPT", "My Clips", "Minecraft", "DESKTOP-ABC123", "example.com", "Funny", "Videos", "VALORANT", "valorant", "discord", "cs2", "Chrome_", "Spaced"];
    for gone in gone {
        assert!(!out.contains(gone), "{gone} left in:\n{out}");
    }
    let kept = [
        "foreground: <app>.exe pid 33636 class <class> rect [2560,-1115,4000,1373]",
        "<path>, done",
        "\"clip\":\"<clip>\",\"new\":3",
        "on <pc> by <user>",
        "<email>",
        "placed: <app> anchor TopCentre (placed by user)",
        "editor: on for <app>\n",
        "editor: saved Right for <app> (previous held 3 more calls)",
    ];
    for kept in kept {
        assert!(out.contains(kept), "{kept} missing:\n{out}");
    }
}

#[test]
fn only_pictures_and_clips_are_taken_by_their_first_bytes() {
    assert_eq!(media::sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]), Some(Kind::Png));
    assert_eq!(media::sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Kind::Jpeg));
    assert_eq!(media::sniff(b"\0\0\0\x18ftypmp42"), Some(Kind::Mp4));
    assert_eq!(media::sniff(b"PK\x03\x04 a zip"), None);
    assert_eq!(media::sniff(b"MZ\x90\0"), None);
    assert!(media::refusal(10, None).is_some());
    assert!(media::refusal(MAX_FILE + 1, Some(Kind::Mp4)).is_some());
    assert!(media::refusal(MAX_FILE, Some(Kind::Mp4)).is_none());
}

fn png_with_text() -> Vec<u8> {
    let chunk = |kind: &[u8], data: &[u8]| {
        let mut c = (data.len() as u32).to_be_bytes().to_vec();
        c.extend(kind);
        c.extend(data);
        let mut crc_in = kind.to_vec();
        crc_in.extend(data);
        c.extend(zip::crc32(&crc_in).to_be_bytes());
        c
    };
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]));
    png.extend(chunk(b"tEXt", b"Author\0someone"));
    png.extend(chunk(b"IDAT", &[1, 2, 3]));
    png.extend(chunk(b"IEND", &[]));
    png
}

#[test]
fn picture_metadata_is_removed() {
    let png = media::clean(Kind::Png, png_with_text()).unwrap();
    assert!(!png.windows(4).any(|w| w == b"tEXt") && png.windows(4).any(|w| w == b"IDAT"));
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, b'J', b'F'];
    jpeg.extend([0xFF, 0xE1, 0, 8, b'E', b'x', b'i', b'f', 0, 0]);
    jpeg.extend([0xFF, 0xFE, 0, 6, b'h', b'i', b'!', b'!']);
    jpeg.extend([0xFF, 0xDA, 0, 2, 9, 9, 0xFF, 0xD9]);
    let clean = media::clean(Kind::Jpeg, jpeg).unwrap();
    assert_eq!(clean, vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, b'J', b'F', 0xFF, 0xDA, 0, 2, 9, 9, 0xFF, 0xD9]);
    assert_eq!(media::clean(Kind::Png, b"\x89PNG\r\n\x1a\nbroken".to_vec()), None);
}

#[test]
fn a_report_zip_holds_the_text_traces_redacted_logs_and_renamed_media() {
    let dir = temp_dir("build");
    let trace = dir.join("trace");
    fs::create_dir_all(&trace).unwrap();
    fs::write(trace.join("island.trace"), "{\"t\":0,\"e\":\"island.state\",\"f\":{\"game\":\"minecraft_java\"}}\n").unwrap();
    fs::write(trace.join("crash-island-1.json"), "{\"kind\":\"panic\"}").unwrap();
    fs::write(trace.join("notes.txt"), "not ours").unwrap();
    let log = dir.join("recall-hdr.log");
    fs::write(&log, "start C:\\Users\\someone\\x.log\n").unwrap();
    let shot = dir.join("My secret name.png");
    fs::write(&shot, png_with_text()).unwrap();
    let input = Input {
        text: "Island missing in Minecraft\nIt never shows.".into(),
        version: "0.1.5".into(),
        windows: "26300".into(),
        trace_dir: Some(trace),
        logs: vec![("recall-hdr.log".into(), log)],
        media: vec![shot],
    };
    let built = build(&input, &dir.join("reports"), &names()).unwrap();
    assert!(built.path.ends_with("report-1.zip"));
    let entries = unzip(&fs::read(&built.path).unwrap());
    let listed: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(listed, ["report.txt", "trace/crash-island-1.json", "trace/island.trace", "logs/recall-hdr.log", "media/media-1.png"]);
    let text = |name: &str| String::from_utf8(entries.iter().find(|(n, _)| n == name).unwrap().1.clone()).unwrap();
    assert!(text("report.txt").starts_with("Recall 0.1.5\nWindows 26300\n\nIsland missing in Minecraft"));
    assert_eq!(text("logs/recall-hdr.log"), "start <path>\n");
    // The second report gets the next number.
    assert!(build(&input, &dir.join("reports"), &names()).unwrap().path.ends_with("report-2.zip"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_report_over_the_cap_is_refused() {
    let dir = temp_dir("cap");
    let mut media = Vec::new();
    for i in 0..2 {
        let clip = dir.join(format!("clip{i}.mp4"));
        let mut data = b"\0\0\0\x18ftypmp42".to_vec();
        data.resize(MAX_FILE as usize, 0);
        fs::write(&clip, data).unwrap();
        media.push(clip);
    }
    let input = Input { media, ..Input::default() };
    assert!(build(&input, &dir.join("reports"), &names()).unwrap_err().contains("over 24 MB"));
    assert!(!dir.join("reports").join("report-1.zip").exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn the_issue_address_carries_only_the_text_and_summary() {
    let input = Input { text: "Overlay & stats gone?".into(), version: "0.1.5".into(), windows: "26300".into(), ..Input::default() };
    let url = issue_url(&input);
    assert!(url.starts_with("https://github.com/LoneBeast27/recall/issues/new?title=Overlay%20%26%20stats%20gone%3F&body="), "{url}");
    assert!(url.contains("Recall%200.1.5%0AWindows%2026300"), "{url}");
}

#[test]
fn clearing_the_trace_removes_only_trace_and_crash_files() {
    let dir = temp_dir("clear");
    for name in ["island.trace", "island.trace.1", "crash-probe-1.json", "crash-probe-native.pending", "keep.mp4", "config.toml"] {
        fs::write(dir.join(name), "x").unwrap();
    }
    assert_eq!(clear_trace(&dir), 4);
    assert!(dir.join("keep.mp4").exists() && dir.join("config.toml").exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn game_tags_pass_the_content_check_but_game_names_do_not() {
    assert_eq!(crate::find_content(r#"{"game":"minecraft_java"}"#, None), None);
    assert!(crate::find_content(r#"{"game":"minecraft_java_modded"}"#, None).is_some());
    assert!(crate::find_content(r#"{"why":"minecraft"}"#, None).is_some());
}
