//! Light copies: the encoder's settings, the stop path, lookup, and a real encode when the
//! machine has FFmpeg with NVENC.
use super::*;
use std::time::SystemTime;

fn scratch(tag: &str) -> crate::test_temp::TempDir {
    let nonce = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    crate::test_temp::TempDir::new(format!("recall-light-{tag}-{}-{nonce}", std::process::id()))
}

#[test]
fn a_light_copy_is_full_size_hevc_at_ten_megabits_without_sound() {
    let args = arguments(Path::new(r"E:\clips\a.mp4"), Path::new(r"C:\cache\light\x.mp4.part"), true);
    let joined = args.join(" ");
    assert!(joined.contains("-hwaccel d3d11va -hwaccel_output_format d3d11 -extra_hw_frames 16"), "GPU-only first: {joined}");
    let through_memory = arguments(Path::new(r"E:\clips\a.mp4"), Path::new(r"C:\cache\light\x.mp4.part"), false).join(" ");
    assert!(through_memory.contains("-hwaccel d3d11va") && !through_memory.contains("hwaccel_output_format"), "{through_memory}");
    for wanted in ["-hwaccel d3d11va", "-threads 4", "-c:v hevc_nvenc", "-b:v 10M", "-maxrate 12500k", "-fps_mode passthrough", "-an", "-map 0:v:0"] {
        assert!(joined.contains(wanted), "{wanted} missing: {joined}");
    }
    assert!(!joined.contains("scale"), "kept at the clip's own resolution (the crosshair survives): {joined}");
    assert_eq!(args.last().map(String::as_str), Some(r"C:\cache\light\x.mp4.part"));
}

#[test]
fn a_short_copy_is_not_complete() {
    assert!(complete(Some(7599), Some(7599)));
    assert!(!complete(Some(7599), Some(7000)), "the old GPU-only failure ended the copy early");
    assert!(!complete(Some(7599), None));
    assert!(complete(None, Some(10)), "a source without a frame count is not held against the copy");
}

#[test]
fn a_stopped_light_copy_leaves_nothing_behind() {
    let (root, clips) = (scratch("root"), scratch("clips"));
    let source = clips.join("a.mp4");
    std::fs::write(&source, vec![0u8; 4096]).unwrap();
    let made = make_in(&root, "aaaa", &source, 2.0, &|| true);
    match made {
        Err(error) => assert!(error == STOPPED || error.contains("ffmpeg") || error.contains("FFmpeg"), "{error}"),
        Ok(path) => panic!("made {path:?} from a stopped run"),
    }
    let left: Vec<_> = std::fs::read_dir(light_dir(&root)).map(|d| d.flatten().map(|e| e.file_name()).collect()).unwrap_or_default();
    assert!(left.is_empty(), "no temp file stays: {left:?}");
    assert!(light_in(&root, &source).is_none());
    let _ = (std::fs::remove_dir_all(&root), std::fs::remove_dir_all(&clips));
}

/// A real encode of a 3 s 1440p clip; skipped where FFmpeg or NVENC is missing.
#[test]
fn a_light_copy_of_a_real_clip_is_found_by_its_clip() {
    let (root, clips) = (scratch("root"), scratch("clips"));
    let source = clips.join("Game__2026-11-03__10-00-00-000__ab.mp4");
    let Ok(ffmpeg) = super::super::binary("RECALL_FFMPEG", "ffmpeg.exe") else {
        eprintln!("ffmpeg not found; skipped");
        return;
    };
    let made = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=2560x1440:rate=60:duration=3", "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .arg(&source)
        .status();
    if !made.is_ok_and(|status| status.success()) {
        eprintln!("could not make a test clip; skipped");
        return;
    }
    let (id, bytes, _) = super::super::identity_of(&source).unwrap();
    match make_in(&root, &id, &source, 3.0, &|| false) {
        Ok(path) => {
            assert_eq!(light_in(&root, &source).as_deref(), Some(path.to_string_lossy().as_ref()));
            let size = path.metadata().unwrap().len();
            assert!(size > 100 && size < bytes.max(5_000_000), "{size} bytes");
        }
        Err(error) if error.contains("nvenc") || error.contains("NVENC") || error.contains("cuda") => eprintln!("no NVENC here ({error}); skipped"),
        Err(error) => panic!("light copy failed: {error}"),
    }
    let _ = (std::fs::remove_dir_all(&root), std::fs::remove_dir_all(&clips));
}
