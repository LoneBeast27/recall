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

/// Java runs any Java app, so `javaw`/`java` is a game only when its window
/// is an LWJGL game window ([`LWJGL_CLASSES`]) or, as before, its title starts
/// with "Minecraft" ("Minecraft 1.21.1", "Minecraft* 1.20.4 - Multiplayer
/// (3rd-party Server)").
const JAVA_STEMS: &[&str] = &["javaw", "java"];
/// Window classes of LWJGL's game window: "GLFW30" (LWJGL 3, Minecraft Java
/// 1.13+, Lunar, Feather and every modpack, whatever their title) and "LWJGL"
/// (LWJGL 2, Minecraft 1.12 and older). Launchers, the dedicated server GUI and
/// Java tools (IntelliJ) use Swing / AWT ("SunAwtFrame") or Qt classes instead.
/// Modpacks rename the window title, never its class.
const LWJGL_CLASSES: &[&str] = &["GLFW30", "LWJGL"];
const MINECRAFT_TITLE: &str = "minecraft";
/// The dedicated server's GUI window ("Minecraft server", `server.jar`
/// without `nogui`) also runs in Java: it is not the game.
const MINECRAFT_SERVER_TITLE: &str = "minecraft server";

/// The foreground window `ev` is a game Recall knows ([`is_known_game_class`]).
pub fn is_known_window(ev: &crate::watcher::WatchEvent) -> bool {
    is_known_game_class(&ev.exe_stem, &ev.title, &ev.window_class)
}

/// `stem` (with `title`, no window class) is a game Recall knows.
pub fn is_known_game(stem: &str, title: &str) -> bool {
    is_known_game_class(stem, title, "")
}

/// `stem` (with the window's `title` and `class`) is a game Recall knows. A
/// Java window is one when its class is LWJGL's or its title starts with
/// "Minecraft", unless it is the dedicated server's GUI.
pub fn is_known_game_class(stem: &str, title: &str, class: &str) -> bool {
    let stem = strip_exe(stem.trim());
    if is_java(stem) {
        let lwjgl = LWJGL_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(class.trim()));
        return (lwjgl || starts_with_ignore_case(title.trim_start(), MINECRAFT_TITLE)) && !is_minecraft_server(stem, title);
    }
    KNOWN_GAME_STEMS.iter().any(|g| g.eq_ignore_ascii_case(stem))
}

/// `stem` is Java and its window a known game ([`is_known_game_class`]).
pub fn is_java_known_game(stem: &str, title: &str, class: &str) -> bool {
    is_java(stem.trim()) && is_known_game_class(stem, title, class)
}

/// Markers in a Java command line that only Minecraft's launch has: the game's
/// and its mod loaders' main classes, the Prism / MultiMC / PolyMC wrapper
/// (they hand the real arguments over stdin), Lunar's, the launcher-brand
/// property and the game's own asset arguments.
const MINECRAFT_COMMAND_MARKERS: &[&str] = &[
    "net.minecraft.",
    "net.minecraftforge.",
    "net.neoforged.",
    "cpw.mods.",
    "net.fabricmc.loader",
    "org.quiltmc.loader",
    "org.prismlauncher.EntryPoint",
    "org.multimc.EntryPoint",
    "org.polymc.EntryPoint",
    "com.moonsworth.lunar",
    "-Dminecraft.launcher.brand=",
    "--assetsDir",
    "--assetIndex",
];

/// A Java game window (see [`is_java_known_game`]) is Minecraft Java Edition:
/// its command line (`None` = unreadable) has a Minecraft marker, or its title
/// starts with "Minecraft". Other LWJGL games (Starsector, libGDX games) keep
/// their own names.
pub fn is_minecraft_java(title: &str, command_line: Option<&str>) -> bool {
    command_line.is_some_and(|c| MINECRAFT_COMMAND_MARKERS.iter().any(|m| c.contains(m)))
        || starts_with_ignore_case(title.trim_start(), MINECRAFT_TITLE)
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
/// The game tag the anonymous action trace may carry for a recording target (`name` as
/// `record_target::Target` names it, `exe` its file name): Minecraft is told apart, every
/// other game is only "listed" or "unknown" (`recall_trace::GAME_TAGS`).
pub fn trace_tag(name: &str, exe: &str) -> &'static str {
    let stem = strip_exe(exe.trim());
    if stem.eq_ignore_ascii_case("Minecraft.Windows") {
        "minecraft_bedrock"
    } else if is_java(stem) && name == crate::record_target::MINECRAFT {
        "minecraft_java"
    } else if KNOWN_GAME_STEMS.iter().any(|g| g.eq_ignore_ascii_case(stem)) {
        "listed_game"
    } else {
        "unknown_game"
    }
}

/// Trace how a Java game window was recognised (or not) as Minecraft: which LWJGL window
/// class it has, whether its title starts with "Minecraft", and whether its command line was
/// readable and named Minecraft. Change-only; no title, class name or command line is kept.
pub fn trace_java(title: &str, class: &str, command_line: Option<&str>) {
    use recall_trace::V;
    let class = class.trim();
    let class = if class.eq_ignore_ascii_case("GLFW30") {
        "glfw30"
    } else if class.eq_ignore_ascii_case("LWJGL") {
        "lwjgl"
    } else {
        "other"
    };
    let command = match command_line {
        None => "unreadable",
        Some(c) if MINECRAFT_COMMAND_MARKERS.iter().any(|m| c.contains(m)) => "minecraft",
        Some(_) => "no_marker",
    };
    let title_says = starts_with_ignore_case(title.trim_start(), MINECRAFT_TITLE);
    let tag = if is_minecraft_java(title, command_line) { "minecraft_java" } else { "unknown_game" };
    recall_trace::act_on_change(
        "detect.java",
        &[("class", V::W(class)), ("title_minecraft", V::B(title_says)), ("command_line", V::W(command)), ("game", V::W(tag))],
    );
}

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
    fn java_with_an_lwjgl_window_is_a_game_whatever_its_title() {
        for title in ["All the Mods 10 - ATM10", "Lunar Client (1.8.9-a1b2c3/master)", "Feather Client", "", "FTB StoneBlock 3"] {
            assert!(is_known_game_class("javaw", title, "GLFW30"), "{title}");
            assert!(is_known_game_class("java.exe", title, "glfw30"), "{title}: any case");
        }
        assert!(is_known_game_class("javaw", "Badlion Minecraft Client", "LWJGL"), "LWJGL 2 (1.12 and older)");
        assert!(!is_known_game_class("javaw", "All the Mods 10 - ATM10", "SunAwtFrame"), "a Swing window");
        assert!(!is_known_game_class("javaw", "IntelliJ IDEA", "SunAwtFrame"));
        assert!(!is_known_game_class("javaw", "ATLauncher", "SunAwtFrame"), "a Java launcher");
        assert!(!is_known_game_class("chrome", "x", "GLFW30"), "the class counts only for Java");
        assert!(!is_known_game_class("javaw", "Minecraft server", "GLFW30"), "the server GUI stays out");
        assert!(is_known_game_class("javaw", "Minecraft 1.21.1", ""), "no class read: the title rule");
    }

    #[test]
    fn minecraft_is_known_by_its_command_line_whatever_its_title() {
        let official = r#""C:\Program Files\Java\bin\javaw.exe" -Dminecraft.launcher.brand=minecraft-launcher -cp x.jar net.minecraft.client.main.Main --version 1.21.1 --gameDir C:\mc --assetsDir C:\mc\assets"#;
        let neoforge = "javaw.exe -p a.jar --add-modules ALL-MODULE-PATH cpw.mods.bootstraplauncher.BootstrapLauncher --launchTarget forgeclient";
        let fabric = "javaw -cp a.jar net.fabricmc.loader.impl.launch.knot.KnotClient --assetIndex 17";
        let prism = r#""C:\Prism\java\bin\javaw.exe" -Xmx4G -Djava.library.path=C:\Prism\instances\x\natives -cp C:\Prism\jars\NewLaunch.jar org.prismlauncher.EntryPoint"#;
        let lunar = "javaw.exe -cp genesis.jar com.moonsworth.lunar.genesis.Genesis --version 1.8.9";
        for cmd in [official, neoforge, fabric, prism, lunar] {
            assert!(is_minecraft_java("All the Mods 10 - ATM10", Some(cmd)), "{cmd}");
        }
        assert!(!is_minecraft_java("Starsector 0.97a", Some("java.exe -cp starfarer.api.jar com.fs.starfarer.StarfarerLauncher")), "another LWJGL game");
        assert!(is_minecraft_java("Minecraft* 1.21.1", Some("javaw.exe -jar custom.jar")), "the title still counts");
        assert!(is_minecraft_java("Minecraft 1.21.1", None), "no command line: the title");
        assert!(!is_minecraft_java("Feather Client", None), "no command line and no title: not named");
    }

    #[test]
    fn the_minecraft_server_gui_is_not_the_game() {
        assert!(!is_known_game("javaw", "Minecraft server"));
        assert!(!is_known_game("java", "Minecraft Server"));
        assert!(is_known_game("javaw", "Minecraft* 1.20.4 - Multiplayer (3rd-party Server)"), "the client on a server");
        assert!(is_known_game("javaw", "Minecraft 1.21.1"));
    }

    #[test]
    fn trace_tags_name_only_minecraft() {
        use crate::record_target::MINECRAFT;
        assert_eq!(trace_tag(MINECRAFT, "javaw.exe"), "minecraft_java");
        assert_eq!(trace_tag("Minecraft", "Minecraft.Windows.exe"), "minecraft_bedrock");
        assert_eq!(trace_tag("Apex Legends", "r5apex.exe"), "listed_game");
        assert_eq!(trace_tag("Some Indie Game", "indie.exe"), "unknown_game");
        assert_eq!(trace_tag("Starsector", "javaw.exe"), "unknown_game", "another LWJGL game");
        for tag in ["minecraft_java", "minecraft_bedrock", "listed_game", "unknown_game"] {
            assert!(recall_trace::GAME_TAGS.contains(&tag));
        }
    }
}
