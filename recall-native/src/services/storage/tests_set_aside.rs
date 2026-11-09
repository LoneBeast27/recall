//! Set-aside ownership lists: at most the newest three are kept, two in one second never
//! overwrite each other, nothing but those exact list names is ever removed, and the files a
//! set-aside left unlisted are adopted again by exact Recall name once a valid list exists.
//! Sandbox folders under the worktree's `.tmp` only.
use std::path::{Path, PathBuf};

use super::*;
use crate::test_temp::TempDir;

const A: &str = "aaaaaaaaaaaaaaaa";

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn cache(tag: &str) -> (TempDir, PathBuf) {
    let sandbox = TempDir::sandbox(format!("recall-set-aside-{tag}"));
    let root = sandbox.join("cache");
    std::fs::create_dir_all(&root).unwrap();
    (sandbox, root)
}

/// Names of the set-aside lists directly in `root`, sorted.
fn set_aside(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("owned.json.corrupt-"))
        .collect();
    names.sort();
    names
}

#[test]
fn only_the_newest_three_set_aside_lists_stay_and_none_overwrites_another() {
    let (_sandbox, root) = cache("keep3");
    // Clips and kept copies that only look like a set-aside list.
    let lookalikes = [
        root.join("owned.json.corrupt-1.mp4"),
        root.join("owned.json.corrupt-old clip"),
        root.join("video").join("owned.json.corrupt-1"),
        root.join("staging").join("kept").join("owned.json.corrupt-1"),
    ];
    for file in &lookalikes {
        write(file, b"a clip");
    }
    let mut contents = Vec::new();
    for round in 0..5 {
        let text = format!("{{\"files\": [ broken {round}");
        write(&root.join("owned.json"), text.as_bytes());
        let _ = owned::listed(&root);
        contents.push(text);
    }
    let kept = set_aside(&root).into_iter().filter(|n| !lookalikes.iter().any(|l| l.file_name().unwrap().to_string_lossy() == *n)).collect::<Vec<_>>();
    assert_eq!(kept.len(), 3, "the newest three set-aside lists stay: {kept:?}");
    let bytes: Vec<String> = kept.iter().map(|n| std::fs::read_to_string(root.join(n)).unwrap()).collect();
    for newest in &contents[2..] {
        assert!(bytes.contains(newest), "rounds 3-5 survive, none overwritten: {bytes:?}");
    }
    for file in &lookalikes {
        assert!(file.exists(), "only exact set-aside list names are ever removed: {}", file.display());
    }
}

#[test]
fn files_a_set_aside_left_unlisted_are_adopted_again_once_a_list_is_valid() {
    let (_sandbox, root) = cache("readopt");
    let copy = root.join("video").join(format!("{A}.mp4"));
    write(&copy, &[9u8; 4096]);
    let user = root.join("video").join("VALORANT__2026-10-22__14-05-09-275__ab12.mp4");
    write(&user, &[9u8; 4096]);
    write(&root.join("owned.json"), b"{ broken");
    assert!(owned::listed(&root).is_empty(), "a corrupt list adopts nothing");
    // The next copy Recall makes starts a valid list again.
    mark_owned(&root, "video/1111111111111111.mp4");
    let listed = owned::listed(&root);
    assert!(listed.contains(&format!("video/{A}.mp4")), "Recall's exact name is adopted again: {listed:?}");
    assert!(!listed.iter().any(|f| f.contains("VALORANT")), "a user's clip never is");
    budget::enforce(&root, 0, 0);
    assert!(!copy.exists() && user.exists(), "so it is evicted again, and the user's clip stays");
}
