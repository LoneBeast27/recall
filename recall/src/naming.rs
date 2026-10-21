//! File names for the engine's non-replay outputs: screenshots and manual long
//! recordings. Same `<Game>__<YYYY-MM-DD>__<HH-MM-SS>` convention (and source
//! sanitizer) as `encoder::clip_path`, so the library parses them the same way.
//!
//! - screenshot: `<out_dir>\screenshots\<Game>__<date>__<time>.png`
//! - long recording: `<out_dir>\<Game>__<date>__<time>__rec.mp4`, written as
//!   `<that>.part` until it is finalized (see `longrec`).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

pub const SCREENSHOT_DIR: &str = "screenshots";
/// Suffix of a finished manual long recording.
pub const LONGREC_SUFFIX: &str = "__rec.mp4";
/// Extension of a file that is still being written.
pub const PART_EXT: &str = ".part";

/// The clip folder (`RECALL_OUT_DIR`, default `V:\ALL RECORDINGS`).
pub fn out_dir() -> String {
    std::env::var("RECALL_OUT_DIR").unwrap_or_else(|_| r"V:\ALL RECORDINGS".to_string())
}

/// Source name -> file-name-safe slug (the `clip_path` rule): characters other
/// than alphanumerics, space, `-` and `_` become `-`; spaces become `-`.
pub fn source_slug(source: &str) -> String {
    let name = source
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .trim()
        .replace(' ', "-");
    if name.is_empty() {
        "Desktop".to_string()
    } else {
        name
    }
}

fn stamp(at: &DateTime<Local>) -> String {
    at.format("%Y-%m-%d__%H-%M-%S").to_string()
}

/// `<dir>\<base><ext>`, else `<base>-2<ext>`, `-3`, ... (first name `taken` rejects).
fn first_free(dir: &Path, base: &str, ext: &str, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(format!("{base}{ext}"));
    if !taken(&first) {
        return first;
    }
    (2..10_000)
        .map(|n| dir.join(format!("{base}-{n}{ext}")))
        .find(|p| !taken(p))
        .unwrap_or(first)
}

/// Where a screenshot taken `at` of `source` goes. `taken` is `Path::exists`
/// in production (tests inject it).
pub fn screenshot_path(out_dir: &str, source: &str, at: &DateTime<Local>, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let dir = Path::new(out_dir).join(SCREENSHOT_DIR);
    first_free(&dir, &format!("{}__{}", source_slug(source), stamp(at)), ".png", taken)
}

/// The FINAL path of a long recording started `at`; it is written to
/// [`part_of`] that path. A name is free only when neither form exists.
pub fn longrec_path(out_dir: &str, source: &str, at: &DateTime<Local>, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let base = format!("{}__{}__rec", source_slug(source), stamp(at));
    first_free(Path::new(out_dir), &base, ".mp4", |p| taken(p) || taken(&part_of(p)))
}

/// `<path>.part`.
pub fn part_of(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(PART_EXT);
    PathBuf::from(s)
}

/// A long recording left in progress (`*__rec.mp4.part`, case-insensitive).
pub fn is_longrec_part(file_name: &str) -> bool {
    file_name.to_ascii_lowercase().ends_with(&format!("{LONGREC_SUFFIX}{PART_EXT}"))
}

/// The name a recovered `.part` gets: the part name without `.part`, or
/// `<stem>-recovered[-N].mp4` when that is taken.
pub fn recovered_path(part: &Path, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let text = part.to_string_lossy();
    let final_path = PathBuf::from(text.strip_suffix(PART_EXT).unwrap_or(&text));
    if !taken(&final_path) {
        return final_path;
    }
    let dir = final_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = final_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    first_free(&dir, &format!("{stem}-recovered"), ".mp4", taken)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 22, 14, 5, 9).single().expect("valid local time")
    }

    #[test]
    fn screenshot_path_follows_the_clip_convention() {
        let p = screenshot_path(r"V:\ALL RECORDINGS", "Rocket League®", &at(), |_| false);
        assert_eq!(p, Path::new(r"V:\ALL RECORDINGS\screenshots\Rocket-League-__2026-10-22__14-05-09.png"));
        let p = screenshot_path(r"V:\clips", "", &at(), |_| false);
        assert!(p.ends_with("screenshots/Desktop__2026-10-22__14-05-09.png"), "{}", p.display());
    }

    #[test]
    fn same_second_screenshots_get_a_counter() {
        let first = screenshot_path(r"D:\c", "VALORANT", &at(), |_| false);
        let p = screenshot_path(r"D:\c", "VALORANT", &at(), |p| p == first);
        assert_eq!(p, Path::new(r"D:\c\screenshots\VALORANT__2026-10-22__14-05-09-2.png"));
    }

    #[test]
    fn longrec_names_skip_existing_final_or_part_files() {
        let free = longrec_path(r"V:\ALL RECORDINGS", "THE FINALS", &at(), |_| false);
        assert_eq!(free, Path::new(r"V:\ALL RECORDINGS\THE-FINALS__2026-10-22__14-05-09__rec.mp4"));
        assert_eq!(part_of(&free), Path::new(r"V:\ALL RECORDINGS\THE-FINALS__2026-10-22__14-05-09__rec.mp4.part"));
        let busy_part = part_of(&free);
        let next = longrec_path(r"V:\ALL RECORDINGS", "THE FINALS", &at(), |p| p == busy_part);
        assert_eq!(next, Path::new(r"V:\ALL RECORDINGS\THE-FINALS__2026-10-22__14-05-09__rec-2.mp4"));
    }

    #[test]
    fn only_long_recording_parts_are_recoverable() {
        assert!(is_longrec_part("VALORANT__2026-10-22__14-05-09__rec.mp4.part"));
        assert!(is_longrec_part("X__2026-10-22__14-05-09__REC.MP4.PART"));
        assert!(!is_longrec_part("VALORANT__2026-10-22__14-05-09-275__abc.mp4.part"), "replay parts are the saver's");
        assert!(!is_longrec_part("VALORANT__2026-10-22__14-05-09__rec.mp4"));
        let part = Path::new(r"V:\c\G__2026-10-22__14-05-09__rec.mp4.part");
        assert_eq!(recovered_path(part, |_| false), Path::new(r"V:\c\G__2026-10-22__14-05-09__rec.mp4"));
        let taken = Path::new(r"V:\c\G__2026-10-22__14-05-09__rec.mp4");
        assert_eq!(
            recovered_path(part, |p| p == taken),
            Path::new(r"V:\c\G__2026-10-22__14-05-09__rec-recovered.mp4")
        );
    }
}
