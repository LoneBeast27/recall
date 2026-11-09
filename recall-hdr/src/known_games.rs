//! Games Recall knows by exe stem. A fullscreen window of one of these skips
//! the render check and commits on the 300 ms rule, the same as a Steam-library
//! exe (`record_render`). A known game is a candidate windowed too (G10,
//! Minecraft Java starts in a window); the deny lists and a `none` override
//! still win (`record_target::is_target` runs first).
//!
//! The one place to extend by hand. The GPU vendor's list (`gpu_games`) is asked
//! after this one, only for exes it does not skip. Stems are matched case-insensitively,
//! with or without `.exe`. Only stems known from the game's own files or from
//! NVIDIA's scan of this PC (`.planning/DETECT_RENDER.md`) are listed.

/// Exe stems of games launched outside Steam (and a few that ship both ways).
const KNOWN_GAME_STEMS: &[&str] = &[
    // Riot
    "VALORANT-Win64-Shipping",
    "League of Legends",
    // Epic
    "FortniteClient-Win64-Shipping",
    "RocketLeague",
    // Battle.net
    "Overwatch",
    "Diablo IV",
    "Diablo III64",
    "Hearthstone",
    "Wow",
    "WowClassic",
    "SC2_x64",
    "cod",
    "ModernWarfare",
    "BlackOpsColdWar",
    // Ubisoft
    "RainbowSix",
    "RainbowSix_Vulkan",
    "ACValhalla",
    "ACOdyssey",
    "ACO",
    "FarCry6",
    "TheCrew2",
    // EA
    "r5apex",
    "r5apex_dx12",
    "FC26",
    "FC25",
    "FC24",
    "FIFA23",
    "BF2042",
    "bf1",
    "bfv",
    "bf4",
    // Others outside Steam
    "cs2",
    "GenshinImpact",
    "Minecraft.Windows",
    "osu!",
];

/// Java runs any Java app, so `javaw`/`java` is Minecraft Java Edition only
/// when its window title starts with "Minecraft" ("Minecraft 1.21.1",
/// "Minecraft* 1.20.4 - Multiplayer (3rd-party Server)").
const JAVA_STEMS: &[&str] = &["javaw", "java"];
const MINECRAFT_TITLE: &str = "minecraft";
/// The dedicated server's GUI window ("Minecraft server", `server.jar`
/// without `nogui`) also runs in Java: it is not the game.
const MINECRAFT_SERVER_TITLE: &str = "minecraft server";

/// `stem` (with `title`) is a game Recall knows.
pub fn is_known_game(stem: &str, title: &str) -> bool {
    let stem = strip_exe(stem.trim());
    if is_java(stem) {
        return starts_with_ignore_case(title.trim_start(), MINECRAFT_TITLE) && !is_minecraft_server(stem, title);
    }
    KNOWN_GAME_STEMS.iter().any(|g| g.eq_ignore_ascii_case(stem))
}

/// `stem` (with `title`) is the Minecraft dedicated server's GUI: never a
/// game, not even fullscreen (`record_target::classify_event`).
pub fn is_minecraft_server(stem: &str, title: &str) -> bool {
    is_java(stem.trim()) && starts_with_ignore_case(title.trim_start(), MINECRAFT_SERVER_TITLE)
}

fn is_java(stem: &str) -> bool {
    let stem = strip_exe(stem);
    JAVA_STEMS.iter().any(|j| j.eq_ignore_ascii_case(stem))
}

/// `"javaw.exe"` -> `"javaw"` (case-insensitive); a bare stem is unchanged.
fn strip_exe(stem: &str) -> &str {
    match stem.len().checked_sub(4) {
        Some(i) if stem.is_char_boundary(i) && stem[i..].eq_ignore_ascii_case(".exe") => &stem[..i],
        _ => stem,
    }
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.is_char_boundary(prefix.len()) && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_stems_match_in_any_case_with_or_without_exe() {
        for s in ["VALORANT-Win64-Shipping", "valorant-win64-shipping", "VALORANT-Win64-Shipping.exe", "FortniteClient-Win64-Shipping", "Overwatch", "RocketLeague", "League of Legends", "r5apex", "RainbowSix", "cod", "Diablo IV", "Hearthstone", "Wow", "FC26", "BF2042"] {
            assert!(is_known_game(s, ""), "{s}");
        }
        for s in ["VALORANT", "chrome", "Discord", "RiotClientServices", "Battle.net", "upc", "EADesktop", "", "  "] {
            assert!(!is_known_game(s, "x"), "{s}");
        }
    }

    #[test]
    fn java_is_minecraft_only_by_title() {
        assert!(is_known_game("javaw", "Minecraft 1.21.1"));
        assert!(is_known_game("javaw.exe", "Minecraft* 1.20.4 - Multiplayer (3rd-party Server)"));
        assert!(is_known_game("java", "minecraft 1.8.9"));
        assert!(!is_known_game("javaw", "IntelliJ IDEA"));
        assert!(!is_known_game("javaw", ""), "empty title");
        assert!(!is_known_game("javaw", "My Minecraft Notes"), "must start with Minecraft");
        assert!(!is_known_game("javaw", "Mine"));
    }

    #[test]
    fn the_minecraft_server_gui_is_not_the_game() {
        assert!(!is_known_game("javaw", "Minecraft server"));
        assert!(!is_known_game("java", "Minecraft Server"));
        assert!(is_known_game("javaw", "Minecraft* 1.20.4 - Multiplayer (3rd-party Server)"), "the client on a server");
        assert!(is_known_game("javaw", "Minecraft 1.21.1"));
    }
}
