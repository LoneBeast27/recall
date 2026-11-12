//! Unit tests: lines are content-free whatever the caller passes, files stay under their cap,
//! and crash records keep only safe text. Every file goes to a fresh temp folder.

use std::fs;
use std::path::PathBuf;

use crate::*;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-trace-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_line_holds_only_safe_words_numbers_and_codes() {
    let line = line(12, "save.done", &[("tracks", 3u32.into()), ("hdr", true.into()), ("ms", V::Ms(812)), ("hr", V::Code(0x8007_0057)), ("why", "disk_full".into())]);
    assert_eq!(line, r#"{"t":12,"e":"save.done","f":{"tracks":3,"hdr":true,"ms":812,"hr":"0x80070057","why":"disk_full"}}"#);
}

#[test]
fn content_passed_by_mistake_becomes_a_question_mark() {
    let leaked: &'static str = Box::leak(String::from(r"C:\Users\someone\Videos\clip 1.mp4").into_boxed_str());
    let line = line(0, "Open Clip.mp4", &[("C:\\path", V::W(leaked)), ("title", V::W("Minecraft 1.21.1"))]);
    assert_eq!(line, r#"{"t":0,"e":"?","f":{"?":"?","title":"?"}}"#);
    assert_eq!(find_content(&line, Some("someone")), None);
}

#[test]
fn safe_words_are_short_lowercase_names() {
    assert_eq!(safe_word("overlay.hidden"), "overlay.hidden");
    assert_eq!(safe_word("game_in_front"), "game_in_front");
    for bad in ["", "Minecraft", "9lives", "a b", "x".repeat(41).as_str(), "naïve", "a\"b", "a\\b"] {
        assert_eq!(safe_word(bad), "?", "{bad:?}");
    }
}

#[test]
fn the_scrubber_finds_paths_names_and_emails() {
    let user = Some("someone");
    let content = [r"C:\Videos", "/Users/x", r"\\server\share", "AppData", "javaw.exe", "clip.mp4", "a@b.com", "hi Someone", r#""game":"minecraft""#, "valorant", "r5apex"];
    for text in content {
        assert!(find_content(text, user).is_some(), "{text:?}");
    }
    let island = r#"{"t":5,"e":"island.state","f":{"shows":"stats","notices_on":true,"game_in_front":true,"desktop":false}}"#;
    for text in [island, "x @ y", "v0.1.2", r#""priority":"stream_upload""#] {
        assert_eq!(find_content(text, user), None, "{text:?}");
    }
}

#[test]
fn a_trace_file_starts_with_the_session_and_stays_under_twice_its_cap() {
    let dir = temp_dir("roll");
    let mut trace = Trace::open("recorder", "0.1.2", Some(dir.clone()), 600);
    for i in 0..100u64 {
        trace.event("save.done", &[("n", i.into())]);
    }
    let live = fs::read_to_string(dir.join("recorder.trace")).unwrap();
    let old = fs::read_to_string(dir.join("recorder.trace.1")).unwrap();
    assert!(live.len() <= 600 && old.len() <= 600, "{} {}", live.len(), old.len());
    assert!(live.ends_with("{\"n\":99}}\n"), "{live}");
    let first = Trace::open("island", "0.0.1", Some(dir.clone()), 600);
    drop(first);
    let island = fs::read_to_string(dir.join("island.trace")).unwrap();
    assert!(island.starts_with(r#"{"t":0,"e":"session.start","f":{"program":"island","version":"v0.0.1"}}"#), "{island}");
    assert!(island.contains(r#""e":"session.day""#), "{island}");
    assert_eq!(find_content(&(live + &old + &island), None), None);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn memory_keeps_the_last_lines_for_a_crash_record() {
    let mut trace = Trace::open("library", "0.1.0", None, TRACE_CAP);
    for i in 0..(RECENT_LINES as u64 + 50) {
        trace.event("tick", &[("n", i.into())]);
    }
    let recent = trace.recent();
    assert_eq!(recent.len(), RECENT_LINES);
    assert!(recent.last().unwrap().contains(&format!("\"n\":{}", RECENT_LINES + 49)));
}

#[test]
fn source_paths_drop_everything_before_the_crate() {
    assert_eq!(source_path("recall-hdr/src/main.rs"), "recall-hdr/src/main.rs");
    assert_eq!(source_path(r"C:\Users\someone\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f\gpui-0.2.2\src\app.rs"), "gpui-0.2.2/src/app.rs");
    assert_eq!(source_path("/home/someone/rust/library/std/src/panicking.rs"), "std/src/panicking.rs");
    assert_eq!(source_path(r"C:\odd place\build.rs"), "build.rs");
    assert_eq!(source_path("weird name/src/a b.rs"), "?");
}

#[test]
fn a_crash_record_holds_codes_and_trace_lines_but_no_message() {
    let lines = vec![line(1, "record.start", &[]), line(2, "save.start", &[("tracks", 2u32.into())])];
    let json = record_json("recorder", "0.1.2", "native", None, Some((0xC000_0005, Some(0x1A2B))), Some("saver"), &lines);
    assert!(json.starts_with(r#"{"kind":"native","program":"recorder","version":"0.1.2","day":""#), "{json}");
    assert!(json.contains(r#""code":"0xC0000005","offset":"0x1A2B","thread":"saver","trace":[{"t":1"#), "{json}");
    let panic = record_json("library", "0.1.0", "panic", Some(r"C:\Users\someone\x.rs:3"), None, Some(r"C:\bad"), &[]);
    assert!(panic.contains(r#""at":"?""#) && panic.contains(r#""thread":"?""#), "{panic}");
    let meter = record_json("recorder", "0.1.2", "panic", None, None, Some("vu-someone headset"), &[]);
    assert!(meter.contains(r#""thread":"vu""#), "{meter}");
    assert_eq!(find_content(&(json + &panic + &meter), Some("someone")), None);
}

#[test]
fn thread_names_keep_only_their_first_word() {
    assert_eq!(thread_word("saver"), "saver");
    assert_eq!(thread_word("vu-someone"), "vu");
    assert_eq!(thread_word("disk lane 2"), "disk");
    assert_eq!(thread_word("Main"), "?");
    assert_eq!(thread_word(""), "?");
}

#[test]
fn a_native_record_is_built_without_formatting_and_filed_at_the_next_start() {
    let prefix = native_prefix("recorder", "0.1.5");
    let mut buf = [0u8; NATIVE_BUF];
    let n = native_bytes(&prefix, 0xC000_0005, Some(0x1A18), &mut buf);
    let text = std::str::from_utf8(&buf[..n]).unwrap().to_string();
    let text = text.as_str();
    assert_eq!(text, r#"{"kind":"native","program":"recorder","version":"0.1.5","code":"0xC0000005","offset":"0x0000000000001A18"}"#);
    let n = native_bytes(&prefix, 0x8000_0003, None, &mut buf);
    assert!(std::str::from_utf8(&buf[..n]).unwrap().ends_with(r#""code":"0x80000003","offset":"other module"}"#));
    // A prefix that would not fit is cut, never written past the buffer.
    let long = vec![b'x'; 400];
    assert!(native_bytes(&long, 1, Some(u64::MAX), &mut buf) <= NATIVE_BUF);

    let dir = temp_dir("pending");
    fs::write(pending_path(&dir, "recorder"), text).unwrap();
    file_pending(&dir, "recorder");
    assert!(!pending_path(&dir, "recorder").exists());
    let kept = crash_records(&dir);
    assert_eq!(kept.len(), 1);
    assert_eq!(fs::read_to_string(&kept[0]).unwrap(), text);
    // A damaged pending file is dropped, not filed.
    fs::write(pending_path(&dir, "recorder"), "garbage").unwrap();
    file_pending(&dir, "recorder");
    assert_eq!(crash_records(&dir).len(), 1);
    assert!(!pending_path(&dir, "recorder").exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn polled_state_is_written_only_when_it_changes() {
    // The process trace in memory only: no file anywhere.
    init("unit", "0.0.0", None);
    let count = |name: &str| crate::trace::snapshot().unwrap().1.iter().filter(|l| l.contains(name)).count();
    for shown in [true, true, true, false, false, true] {
        act_on_change("unit.state", &[("shown", shown.into())]);
    }
    assert_eq!(count("\"unit.state\""), 3);
}

#[test]
fn test_runs_never_write_the_users_trace() {
    let vars = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
    let local = &[("LOCALAPPDATA", r"C:\L")];
    assert_eq!(crate::dir_from(vars(local)), Some(PathBuf::from(r"C:\L").join("Recall").join("trace")));
    assert_eq!(crate::dir_from(vars(&[("LOCALAPPDATA", r"C:\L"), ("RECALL_INSTANCE_TAG", "check1")])), None);
    assert_eq!(crate::dir_from(vars(&[("RECALL_INSTANCE_TAG", "check1"), ("RECALL_TRACE_DIR", r"D:\t")])), Some(PathBuf::from(r"D:\t")));
    assert_eq!(crate::dir_from(vars(&[("LOCALAPPDATA", r"C:\L"), ("RECALL_TRACE_DIR", "OFF")])), None);
    // The viewer checks start the app offscreen with no instance tag.
    assert_eq!(crate::dir_from(vars(&[("LOCALAPPDATA", r"C:\L"), ("RECALL_NATIVE_OFFSCREEN", "1")])), None);
    assert_eq!(crate::dir_from(vars(&[("LOCALAPPDATA", r"C:\L"), ("RECALL_NO_CONTROLLER", "1")])), None);
}

#[test]
fn crash_records_keep_the_newest_ten() {
    let dir = temp_dir("crash");
    for i in 0..(KEEP + 3) {
        write_crash_record(&dir, if i % 2 == 0 { "island" } else { "library" }, &format!("{{\"n\":{i}}}")).unwrap();
    }
    let kept = crash_records(&dir);
    assert_eq!(kept.len(), KEEP);
    assert!(kept.last().unwrap().ends_with(format!("crash-island-{}.json", KEEP + 3)), "{kept:?}");
    assert_eq!(fs::read_to_string(kept.last().unwrap()).unwrap(), format!("{{\"n\":{}}}", KEEP + 2));
    assert_eq!(fs::read_to_string(&kept[0]).unwrap(), "{\"n\":3}");
    let _ = fs::remove_dir_all(dir);
}
