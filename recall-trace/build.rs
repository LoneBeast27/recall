//! Builds the report vocabulary: every word that appears in a string literal of Recall's own
//! code (the recorder, the engine, the island and the app), lower-cased, one per line, in
//! `OUT_DIR/vocab.txt`. A report keeps a log word only when it is in this list, so a game,
//! clip, device or person's name (none of which are in Recall's code) is replaced. Files that
//! hold tables of game, launcher or app names are left out, and so are tests.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Source folders, relative to this crate.
const ROOTS: &[&str] = &["../recall-hdr/src", "../recall/src", "../recall-overlay/src", "../recall-native/src"];

/// Files whose literals are names of games, launchers or other programs.
const SKIP_FILES: &[&str] = &["known_games.rs", "private_app_stems.rs", "private_apps.rs", "launcher_libraries.rs", "launcher_libraries_win.rs", "game_library.rs", "gpu_games.rs", "game_apps.rs"];

/// Never kept, wherever they appear (a sample of game and launcher words, as `scrub.rs`).
const NEVER: &[&str] = &[
    "minecraft", "javaw", "valorant", "fortnite", "apex", "r5apex", "cs2", "csgo", "dota", "overwatch", "league", "roblox", "steam", "epicgames", "riot", "eldenring",
    "cyberpunk", "warzone", "genshin", "genshinimpact", "rocketleague", "pubg", "destiny", "halo", "gta", "rdr2", "discord", "brave", "chrome", "firefox", "spotify", "chatgpt",
    "epic", "gog", "ubisoft", "battle", "itch", "lunar", "feather", "prism", "multimc", "polymc", "forge", "fabric", "quilt", "neoforged", "badlion", "starsector",
    "rocket", "legends", "marvel", "rivals", "counter", "strike", "siege", "rainbow", "shipping", "win64", "dx11", "dx12", "witcher", "baldur", "warframe", "tarkov",
    "deadlock", "helldivers", "palworld", "starfield", "skyrim", "fallout", "diablo", "hearthstone", "warcraft", "forza", "turismo", "fifa", "battlefield", "osu",
];

/// File names that hold sample text (previews, smoke fixtures, demo notices), not log lines.
const SKIP_PARTS: &[&str] = &["preview", "smoke", "export", "filmstrip", "fixture", "sample", "demo"];

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let mut words = BTreeSet::new();
    for root in ROOTS {
        let dir = manifest.join(root);
        println!("cargo:rerun-if-changed={}", dir.display());
        collect(&dir, &mut words);
    }
    // A word holding a game or program word anywhere ("marvelwin64shipping") goes too.
    // Short words (4 letters or fewer: "itch", "osu") only as the whole word, so "switch" stays.
    words.retain(|w| !NEVER.iter().any(|never| if never.len() <= 4 { w == never } else { w.contains(never) }));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("vocab.txt");
    let text: Vec<&str> = words.iter().map(String::as_str).collect();
    fs::write(out, text.join("\n")).unwrap();
}

fn collect(dir: &Path, words: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            collect(&path, words);
        } else if name.ends_with(".rs") && !name.contains("test") && !SKIP_FILES.contains(&name.as_str()) && !SKIP_PARTS.iter().any(|p| name.contains(p)) {
            if let Ok(text) = fs::read_to_string(&path) {
                literals(&text, words);
            }
        }
    }
}

/// Words inside the file's string literals (`"…"`), stopping at a `#[cfg(test)]` module.
fn literals(text: &str, words: &mut BTreeSet<String>) {
    // The test module (not a single #[cfg(test)] item) ends the scan.
    let end = ["#[cfg(test)]\nmod ", "#[cfg(test)]\r\nmod "].iter().filter_map(|m| text.find(m)).min().unwrap_or(text.len());
    let text = &text[..end];
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                // A comment: skip to the line end.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'\'' if bytes.get(i + 2) == Some(&b'\'') || (bytes.get(i + 1) == Some(&b'\\') && bytes.get(i + 3) == Some(&b'\'')) => {
                i += if bytes.get(i + 1) == Some(&b'\\') { 4 } else { 3 }; // a char literal such as '"'
            }
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                add_words(&text[start..i.min(text.len())], words);
                i += 1;
            }
            _ => i += 1,
        }
    }
}

fn add_words(literal: &str, words: &mut BTreeSet<String>) {
    let mut word = String::new();
    for c in literal.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_alphanumeric() || (c == '_' && !word.is_empty()) {
            word.push(c);
        } else {
            if word.len() >= 2 && word.as_bytes()[0].is_ascii_alphabetic() {
                words.insert(word.to_ascii_lowercase());
            }
            word.clear();
        }
    }
}
