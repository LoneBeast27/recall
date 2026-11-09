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

/// The clip folder (`RECALL_OUT_DIR`, which the controller always sets; run by hand,
/// `%USERPROFILE%\Videos\Recall`).
pub fn out_dir() -> String {
    std::env::var("RECALL_OUT_DIR").unwrap_or_else(|_| {
        let home = crate::user_dirs::profile();
        crate::user_dirs::checked(home.join("Videos").join("Recall")).to_string_lossy().into_owned()
    })
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

/// `RECALL_GAME_FOLDERS=1` (Settings → Clip folders → "Sort clips into game folders"; the
/// controller sets it from `recording.game_folders`): new clips and long recordings go into
/// `<out_dir>\<Game>\` instead of `<out_dir>` itself. Clips already saved never move.
pub const GAME_FOLDERS_ENV: &str = "RECALL_GAME_FOLDERS";

/// Whether new recordings go into per-game folders (read at each save: cheap, and a test can set it).
pub fn game_folders_on() -> bool {
    std::env::var(GAME_FOLDERS_ENV).is_ok_and(|v| v.trim() == "1")
}

/// Folder names Recall itself uses inside the clip folder; a game never gets one of these.
const RESERVED_FOLDERS: &[&str] = &["thumbs", "screenshots", "exports", "edited", "raw", "kept", "staging"];
/// Longest game folder name, in characters (Windows paths stay short).
const GAME_FOLDER_MAX: usize = 60;

/// Game name -> a folder name that is safe on Windows and readable: "Rocket League®" ->
/// "Rocket League". Keeps letters, digits, spaces and a few harmless marks; drops the rest
/// (`<>:"/\|?*`, control characters, symbols); collapses spaces; no leading or trailing dots
/// or spaces; at most [`GAME_FOLDER_MAX`] characters; a Windows device name (`CON`, `COM1`, ...)
/// gets a `_` in front, one of Recall's own folder names a `_` on the end. Empty -> "Desktop".
pub fn game_folder_name(source: &str) -> String {
    let kept: String = source
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|&c| c.is_alphanumeric() || " -_.,'&()[]!+#=~;".contains(c))
        .collect();
    let mut name = kept.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    if name.chars().count() > GAME_FOLDER_MAX {
        name = name.chars().take(GAME_FOLDER_MAX).collect();
    }
    let name = name.trim_matches(|c: char| c == '.' || c == ' ').to_string();
    if name.is_empty() {
        return "Desktop".to_string();
    }
    let lower = name.to_ascii_lowercase();
    let base = lower.split('.').next().unwrap_or("").trim_end();
    let device = matches!(base, "con" | "prn" | "aux" | "nul")
        || ((base.starts_with("com") || base.starts_with("lpt")) && base.len() == 4 && base.as_bytes()[3].is_ascii_digit());
    if device {
        return format!("_{name}");
    }
    if RESERVED_FOLDERS.contains(&lower.as_str()) {
        return format!("{name}_");
    }
    name
}

/// The folder a new clip of `source` goes into: `<out_dir>\<Game>` with game folders `on`, else
/// `out_dir`.
pub fn clip_dir_with(out_dir: &str, source: &str, on: bool) -> String {
    if on {
        Path::new(out_dir).join(game_folder_name(source)).to_string_lossy().into_owned()
    } else {
        out_dir.to_string()
    }
}

/// [`clip_dir_with`] with the live setting ([`game_folders_on`]).
pub fn clip_dir(out_dir: &str, source: &str) -> String {
    clip_dir_with(out_dir, source, game_folders_on())
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

/// The next part of the long recording that began at `first` (its stream
/// changed): `<Game>__<date>__<time>__rec-2.mp4`, then `-3`, ... The first
/// name with neither its final nor its `.part` form on disk, so an existing
/// clip is never overwritten.
pub fn longrec_next_part(first: &Path, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let dir = first.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = first.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // A same-second `__rec-N` keeps its `__rec` base, so every part stays recoverable.
    let base = match stem.rsplit_once('-') {
        Some((head, n)) if head.ends_with("__rec") && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => head.to_string(),
        _ => stem,
    };
    (2..10_000)
        .map(|n| dir.join(format!("{base}-{n}.mp4")))
        .find(|p| !taken(p) && !taken(&part_of(p)))
        .unwrap_or_else(|| first_free(&dir, &format!("{base}-part"), ".mp4", |p| taken(p) || taken(&part_of(p))))
}

/// `<path>.part`.
pub fn part_of(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(PART_EXT);
    PathBuf::from(s)
}

/// A long recording left in progress (`*__rec.mp4.part`, or a same-second
/// `*__rec-N.mp4.part`; case-insensitive).
pub fn is_longrec_part(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(PART_EXT).and_then(|s| s.strip_suffix(".mp4")) else { return false };
    let stem = match stem.rsplit_once('-') {
        Some((head, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => stem,
    };
    LONGREC_SUFFIX.strip_suffix(".mp4").is_some_and(|rec| stem.ends_with(rec))
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
    fn game_folder_names_are_safe_and_readable() {
        assert_eq!(game_folder_name("Rocket League®"), "Rocket League");
        assert_eq!(game_folder_name("Tom Clancy's Rainbow Six® Siege"), "Tom Clancy's Rainbow Six Siege");
        assert_eq!(game_folder_name("Counter-Strike 2"), "Counter-Strike 2");
        assert_eq!(game_folder_name(r#"a<b>c:d"e/f\g|h?i*j"#), "abcdefghij");
        assert_eq!(game_folder_name("  THE   FINALS \t"), "THE FINALS");
        assert_eq!(game_folder_name("Game..."), "Game");
        assert_eq!(game_folder_name("..hidden"), "hidden");
        assert_eq!(game_folder_name(""), "Desktop");
        assert_eq!(game_folder_name("™®"), "Desktop");
        assert_eq!(game_folder_name("..."), "Desktop");
        assert_eq!(game_folder_name("CON"), "_CON");
        assert_eq!(game_folder_name("com1"), "_com1");
        assert_eq!(game_folder_name("nul.txt"), "_nul.txt");
        assert_eq!(game_folder_name("Console"), "Console");
        assert_eq!(game_folder_name("Thumbs"), "Thumbs_");
        assert_eq!(game_folder_name("screenshots"), "screenshots_");
        assert_eq!(game_folder_name("Line\nBreak\u{7}"), "Line Break");
        assert_eq!(game_folder_name("原神"), "原神");
        let long = "x".repeat(200);
        assert_eq!(game_folder_name(&long).chars().count(), GAME_FOLDER_MAX);
        assert_eq!(game_folder_name(&format!("{} .end", "y".repeat(59))), "y".repeat(59));
    }

    #[test]
    fn clip_dir_is_the_game_folder_only_when_on() {
        assert_eq!(clip_dir_with(r"E:\Clips", "VALORANT", false), r"E:\Clips");
        assert_eq!(Path::new(&clip_dir_with(r"E:\Clips", "VALORANT", true)), Path::new(r"E:\Clips").join("VALORANT"));
        assert_eq!(Path::new(&clip_dir_with(r"E:\Clips", "", true)), Path::new(r"E:\Clips").join("Desktop"));
    }

    #[test]
    fn screenshot_path_follows_the_clip_convention() {
        let p = screenshot_path(r"E:\Clips", "Rocket League®", &at(), |_| false);
        assert_eq!(p, Path::new(r"E:\Clips\screenshots\Rocket-League-__2026-10-22__14-05-09.png"));
        let p = screenshot_path(r"E:\clips", "", &at(), |_| false);
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
        let free = longrec_path(r"E:\Clips", "THE FINALS", &at(), |_| false);
        assert_eq!(free, Path::new(r"E:\Clips\THE-FINALS__2026-10-22__14-05-09__rec.mp4"));
        assert_eq!(part_of(&free), Path::new(r"E:\Clips\THE-FINALS__2026-10-22__14-05-09__rec.mp4.part"));
        let busy_part = part_of(&free);
        let next = longrec_path(r"E:\Clips", "THE FINALS", &at(), |p| p == busy_part);
        assert_eq!(next, Path::new(r"E:\Clips\THE-FINALS__2026-10-22__14-05-09__rec-2.mp4"));
    }

    #[test]
    fn a_continued_recording_takes_the_next_free_part_name() {
        let first = Path::new(r"E:\c\G__2026-10-22__14-05-09__rec.mp4");
        let n = |name: &str| Path::new(r"E:\c").join(name);
        assert_eq!(longrec_next_part(first, |p| p == first), n("G__2026-10-22__14-05-09__rec-2.mp4"));
        // Part 2 is on disk, part 3 is being written by another recording: part 4.
        let busy = [first.to_path_buf(), n("G__2026-10-22__14-05-09__rec-2.mp4"), part_of(&n("G__2026-10-22__14-05-09__rec-3.mp4"))];
        assert_eq!(longrec_next_part(first, |p| busy.iter().any(|b| b == p)), n("G__2026-10-22__14-05-09__rec-4.mp4"));
        // A same-second second recording (`rec-2`) continues under the same base, still recoverable.
        let second = n("G__2026-10-22__14-05-09__rec-2.mp4");
        let next = longrec_next_part(&second, |p| p == first || p == second);
        assert_eq!(next, n("G__2026-10-22__14-05-09__rec-3.mp4"));
        assert!(is_longrec_part(&part_of(&next).file_name().unwrap().to_string_lossy()));
    }

    #[test]
    fn only_long_recording_parts_are_recoverable() {
        assert!(is_longrec_part("VALORANT__2026-10-22__14-05-09__rec.mp4.part"));
        assert!(is_longrec_part("X__2026-10-22__14-05-09__REC.MP4.PART"));
        assert!(is_longrec_part("G__2026-10-22__14-05-09__rec-2.mp4.part"), "the counter name longrec_path picks");
        assert!(is_longrec_part("G__2026-10-22__14-05-09__rec-12.MP4.part"));
        assert!(!is_longrec_part("G__2026-10-22__14-05-09__rec-.mp4.part"));
        assert!(!is_longrec_part("G__2026-10-22__14-05-09__rec-x2.mp4.part"));
        assert!(!is_longrec_part("VALORANT__2026-10-22__14-05-09-275__abc.mp4.part"), "replay parts are the saver's");
        assert!(!is_longrec_part("VALORANT__2026-10-22__14-05-09__rec.mp4"));
        let part = Path::new(r"E:\c\G__2026-10-22__14-05-09__rec.mp4.part");
        assert_eq!(recovered_path(part, |_| false), Path::new(r"E:\c\G__2026-10-22__14-05-09__rec.mp4"));
        let taken = Path::new(r"E:\c\G__2026-10-22__14-05-09__rec.mp4");
        assert_eq!(
            recovered_path(part, |p| p == taken),
            Path::new(r"E:\c\G__2026-10-22__14-05-09__rec-recovered.mp4")
        );
        let second = Path::new(r"E:\c\G__2026-10-22__14-05-09__rec-2.mp4.part");
        assert_eq!(recovered_path(second, |_| false), Path::new(r"E:\c\G__2026-10-22__14-05-09__rec-2.mp4"));
    }
}
