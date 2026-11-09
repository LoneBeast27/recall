//! "Sort clips into game folders" and the storage limit: only folders Recall made (marker, a
//! real folder, not hidden or system) count. Any other subfolder holding Recall-named clips
//! is the user's: never scanned, never recycled. Temp folders only; nothing is recycled.

use super::*;
use crate::game_folder_rule as rule;

struct Gone(PathBuf);
impl Drop for Gone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> (PathBuf, Gone) {
    let dir = std::env::temp_dir().join(format!("recall-storage-games-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    (dir.clone(), Gone(dir))
}

fn game_folder(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    rule::write_marker(dir).unwrap();
}

fn names(l: &Listing) -> Vec<String> {
    let mut clips: Vec<String> = l.clips.iter().map(|c| c.name.clone()).collect();
    clips.sort();
    clips
}

const CLIP: &str = "G__2026-10-22__16-00-00.mp4";

/// Clips in a marked game folder count and go with their own thumbnails and sidecars; Recall's
/// own subfolders, unmarked folders and anything deeper never count.
#[test]
fn scan_counts_marked_game_folder_clips_one_level_down() {
    let (dir, _gone) = temp("marked");
    let game = dir.join("Rocket League");
    game_folder(&game);
    for d in [game.join("thumbs"), game.join("old"), dir.join("screenshots"), dir.join("exports"), dir.join(".hidden")] {
        std::fs::create_dir_all(d).unwrap();
    }
    let top = "Desktop__2026-10-22__14-05-09.mp4";
    let rl = "Rocket-League__2026-10-22__15-00-00-275__ab12.mp4";
    std::fs::write(dir.join(top), vec![0u8; 10]).unwrap();
    std::fs::write(game.join(rl), vec![0u8; 20]).unwrap();
    std::fs::write(game.join("Rocket-League__2026-10-22__15-00-00-275__ab12.recall-marks.json"), b"{}").unwrap();
    std::fs::write(game.join("thumbs").join("Rocket-League__2026-10-22__15-00-00-275__ab12-1783463424-thumb.jpg"), b"j").unwrap();
    for d in [game.join("old"), dir.join("screenshots"), dir.join("exports"), dir.join(".hidden")] {
        std::fs::write(d.join(CLIP), vec![0u8; 99]).unwrap();
    }
    let l = scan(&dir).unwrap();
    let rel = format!("Rocket League/{rl}");
    assert_eq!(names(&l), vec![top.to_string(), rel.clone()]);
    assert_eq!(at(&dir, &rel), game.join(rl));
    assert_eq!(
        companions(&dir, &rel, &l),
        vec![
            game.join(rl),
            game.join("Rocket-League__2026-10-22__15-00-00-275__ab12.recall-marks.json"),
            game.join("thumbs").join("Rocket-League__2026-10-22__15-00-00-275__ab12-1783463424-thumb.jpg"),
        ]
    );
    assert_eq!(companions(&dir, top, &l), vec![dir.join(top)], "a game folder's thumbnails never go with a top-level clip");
}

/// Option off (or on): `<out>\Keepers\` and a pre-existing `<out>\VALORANT\` the user made hold
/// Recall-named clips. Not scanned, so a full drive never recycles them.
#[test]
fn an_unmarked_folder_is_never_scanned_or_recycled() {
    let (dir, _gone) = temp("unmarked");
    for name in ["Keepers", "VALORANT"] {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join(CLIP), vec![0u8; 99]).unwrap();
    }
    game_folder(&dir.join("VALORANT (Recall)"));
    std::fs::write(dir.join("VALORANT (Recall)").join("VALORANT__2026-10-22__17-00-00.mp4"), vec![0u8; 5]).unwrap();
    let l = scan(&dir).unwrap();
    assert_eq!(names(&l), vec!["VALORANT (Recall)/VALORANT__2026-10-22__17-00-00.mp4".to_string()]);
    let p = plan(&l, &HashSet::new(), 0, u64::MAX / 2);
    assert!(p.remove.iter().all(|n| !n.starts_with("Keepers/") && !n.starts_with("VALORANT/")), "{:?}", p.remove);
}

/// A junction carrying a marker is not a game folder: its target is somewhere else.
#[test]
fn a_linked_game_folder_is_skipped() {
    let (dir, _gone) = temp("link");
    let (target, _target_gone) = temp("link-target");
    rule::write_marker(&target).unwrap();
    std::fs::write(target.join(CLIP), vec![0u8; 99]).unwrap();
    if !rule::junction_for_test(&target, &dir.join("Apex Legends")) {
        eprintln!("skipped: could not make a junction");
        return;
    }
    assert!(scan(&dir).unwrap().clips.is_empty());
}

/// A marked folder the user hid is skipped, as the library skips it.
#[test]
fn a_hidden_game_folder_is_skipped() {
    let (dir, _gone) = temp("hidden");
    let game = dir.join("Rocket League");
    game_folder(&game);
    std::fs::write(game.join(CLIP), vec![0u8; 99]).unwrap();
    assert_eq!(scan(&dir).unwrap().clips.len(), 1);
    if !rule::hide_for_test(&game) {
        eprintln!("skipped: could not hide the folder");
        return;
    }
    assert!(scan(&dir).unwrap().clips.is_empty());
}

#[test]
fn game_folder_clips_keep_favourites_and_edits() {
    let gib = |name: &str, modified| ClipFile { name: name.into(), bytes: 3 * GIB, modified };
    let clips = vec![gib("A/G__2026-10-22__14-05-09.mp4", 100), gib("B/G__2026-10-22__14-05-10.mp4", 200), gib("G__2026-10-22__14-05-11.mp4", 300), gib("G__2026-10-22__14-05-12.mp4", 400)];
    let mut names: Vec<String> = clips.iter().map(|c| c.name.clone()).collect();
    names.push("B/G__2026-10-22__14-05-10.mp4.recall-edit.json".into());
    let l = Listing { clips, names, thumbs: Vec::new() };
    let favs: HashSet<String> = ["g__2026-10-22__14-05-09.mp4".to_string()].into();
    let p = plan(&l, &favs, 3 * GIB, 1_000_000);
    assert_eq!(p.remove, vec!["G__2026-10-22__14-05-11.mp4".to_string()], "A's clip is a favourite, B's has edits, the newest stays");
}

/// (a) "Use this folder": a Recall-named clip already in the user's folder is never counted and
/// never in `plan.remove`, even at a tiny limit; clips saved there after adoption are.
#[test]
fn an_adopted_folders_existing_clips_are_never_counted_or_recycled() {
    let (dir, _gone) = temp("adopted");
    let game = dir.join("VALORANT");
    std::fs::create_dir_all(&game).unwrap();
    let old = "VALORANT__2025-01-01__10-00-00.mp4";
    std::fs::write(game.join(old), vec![0u8; 50]).unwrap();
    rule::adopt(&game).unwrap();
    let new = ["VALORANT__2026-11-10__04-00-00-000__ab.mp4", "VALORANT__2026-11-10__05-00-00-000__cd.mp4"];
    for name in new {
        std::fs::write(game.join(name), vec![0u8; 20]).unwrap();
    }
    let l = scan(&dir).unwrap();
    assert_eq!(names(&l), new.iter().map(|n| format!("VALORANT/{n}")).collect::<Vec<_>>(), "the existing clip is not counted");
    let p = plan(&l, &HashSet::new(), 1, u64::MAX / 2);
    assert_eq!(p.total, 40, "only the new clips count");
    assert!(!p.remove.iter().any(|n| n.ends_with(old)), "{:?}", p.remove);
    assert!(!p.remove.is_empty(), "a clip saved after adoption may go: {:?}", p.remove);
    // A marker that can't be parsed: the folder is not a game folder, nothing in it counts.
    std::fs::write(game.join(rule::MARKER), b"{broken").unwrap();
    assert!(scan(&dir).unwrap().clips.is_empty());
}
