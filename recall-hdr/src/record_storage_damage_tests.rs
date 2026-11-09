//! The storage cap after the app found library.json damaged (deep-audit R2-1, R2-2):
//! a former favourite is never recycled. Temp folders only; nothing is recycled.
use super::*;

const FAV: &str = "G__2026-10-22__14-05-09.mp4";
const OTHER: &str = "G__2026-10-23__10-00-00.mp4";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recall-storage-damage-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn set_mtime(path: &Path, unix: u64) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(unix)).unwrap();
}

/// What the app leaves after damage, a restart and a duration flush: the truncated
/// file as `.bad`, then a fresh library.json (written later) without the favourites.
fn damaged_then_rewritten(dir: &Path) -> PathBuf {
    let lib = dir.join("library.json");
    let bad = dir.join("library.json.bad-2026-11-07");
    std::fs::write(&bad, format!(r#"{{"{FAV}":{{"favourite":true,"title":"Ace"}},"{OTHER}":{{"dura"#)).unwrap();
    set_mtime(&bad, 1_000);
    std::fs::write(&lib, format!(r#"{{"{OTHER}":{{"duration":{{"bytes":1,"mtime":2,"secs":3.0}}}}}}"#)).unwrap();
    set_mtime(&lib, 2_000);
    lib
}

#[test]
fn a_former_favourite_in_a_bad_copy_is_never_recycled() {
    let dir = temp_dir("union");
    let lib = damaged_then_rewritten(&dir);
    let favs = favourites_at(&lib).expect("library.json is newer than every .bad: readable");
    assert!(favs.contains(&FAV.to_lowercase()), "the .bad copy's favourite still counts: {favs:?}");
    // Cap run over a folder holding both clips: the former favourite stays.
    let clips = vec![ClipFile { name: FAV.into(), bytes: 5 * GIB, modified: 100 }, ClipFile { name: OTHER.into(), bytes: 5 * GIB, modified: 200 }, ClipFile { name: "G__2026-10-24__10-00-00.mp4".into(), bytes: GIB, modified: 300 }];
    let names = clips.iter().map(|c| c.name.clone()).collect();
    let p = plan(&Listing { clips, names, thumbs: vec![] }, &favs, GIB, 1_000_000);
    assert!(!p.remove.contains(&FAV.to_string()), "{:?}", p.remove);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unconfirmed_damage_notice_keeps_the_cap_paused_after_a_star_edit() {
    let dir = temp_dir("hold");
    let lib = damaged_then_rewritten(&dir);
    // The user starred a clip (library.json written after the .bad) but has not
    // pressed "Keep going" on the notice: the app keeps library.json.hold.
    std::fs::write(dir.join("library.json.hold"), b"library.json.bad-2026-11-07").unwrap();
    let err = favourites_at(&lib).expect_err("the user has not confirmed the damage notice");
    assert!(err.contains("library.json.hold"), "{err}");
    std::fs::remove_file(dir.join("library.json.hold")).unwrap();
    assert!(favourites_at(&lib).is_ok(), "confirmed: the cap runs again");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreadable_bad_copy_leaves_favourites_unknown() {
    let dir = temp_dir("unreadable");
    let lib = damaged_then_rewritten(&dir);
    use std::os::windows::fs::OpenOptionsExt;
    let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(dir.join("library.json.bad-2026-11-07")).unwrap();
    assert!(favourites_at(&lib).is_err(), "a .bad that can't be read may hold favourites");
    drop(held);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_path_keyed_favourite_in_a_bad_copy_protects_the_clip_by_name() {
    // library L14 keys library.json by full path; a .bad written after that holds path keys,
    // and the plan looks clips up by file name.
    let dir = temp_dir("path-key");
    let lib = dir.join("library.json");
    let bad = dir.join("library.json.bad-2026-11-07");
    std::fs::write(&bad, format!(r#"{{"v:\\recall\\{}":{{"favourite":true}},"x":{{"dura"#, FAV.to_lowercase())).unwrap();
    set_mtime(&bad, 1_000);
    std::fs::write(&lib, b"{}").unwrap();
    set_mtime(&lib, 2_000);
    let favs = favourites_at(&lib).expect("readable");
    assert!(favs.contains(&FAV.to_lowercase()), "the .bad copy's path-keyed favourite keeps the clip: {favs:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
