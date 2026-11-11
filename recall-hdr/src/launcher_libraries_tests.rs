use super::*;
use crate::config::Config;
use crate::game_library::{game_library_in, is_game_library_path_in, Library};
use crate::watcher::WatchEvent;
use std::collections::HashMap;

/// Subkeys of one registry key: (name, values).
type Subkeys = Vec<(String, HashMap<String, String>)>;

/// Injected launcher state: no real registry, files or environment.
#[derive(Default)]
struct Fixture {
    env: HashMap<String, String>,
    dirs: HashMap<String, Vec<String>>,
    files: HashMap<String, String>,
    keys: HashMap<String, Subkeys>,
}

impl Fixture {
    fn file(mut self, dir: &str, name: &str, text: &str) -> Self {
        self.dirs.entry(dir.to_string()).or_default().push(name.to_string());
        self.files.insert(format!("{dir}\\{name}"), text.to_string());
        self
    }
    /// `game` in `parent` with itch's install receipt (`.itch\receipt.json.gz`).
    fn receipt(mut self, parent: &str, game: &str) -> Self {
        self.dirs.entry(parent.to_string()).or_default().push(game.to_string());
        self.dirs.entry(format!("{parent}\\{game}\\.itch")).or_default().push("receipt.json.gz".to_string());
        self
    }
    fn key(mut self, path: &str, sub: &str, values: &[(&str, &str)]) -> Self {
        let vals = values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        self.keys.entry(path.to_string()).or_default().push((sub.to_string(), vals));
        self
    }
}

impl LibrarySources for Fixture {
    fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }
    fn list_dir(&self, dir: &str) -> Vec<String> {
        self.dirs.get(dir).cloned().unwrap_or_default()
    }
    fn read_file(&self, path: &str) -> Option<String> {
        self.files.get(path).cloned()
    }
    fn reg_subkeys(&self, _hive: Hive, path: &str, values: &[&str]) -> Vec<(String, Vec<Option<String>>)> {
        let subs = self.keys.get(path).cloned().unwrap_or_default();
        subs.into_iter().map(|(k, vals)| (k, values.iter().map(|v| vals.get(*v).cloned()).collect())).collect()
    }
}

const EPIC: &str = "X:\\PD\\Epic\\EpicGamesLauncher\\Data\\Manifests";
const GOG: &str = "SOFTWARE\\WOW6432Node\\GOG.com\\Games";
const ITCH: &str = "X:\\Users\\me\\AppData\\Roaming\\itch";
const UNINSTALL: &str = "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall";

/// One install per launcher, on made-up drives (nothing hard-coded in the code).
fn machine() -> Fixture {
    let mut f = Fixture::default();
    f.env.insert("ProgramData".into(), "X:\\PD".into());
    f.env.insert("APPDATA".into(), "X:\\Users\\me\\AppData\\Roaming".into());
    f.env.insert("ProgramFiles".into(), "X:\\Program Files".into());
    f.env.insert("USERPROFILE".into(), "X:\\Users\\me".into());
    let itch = "X:\\Users\\me\\AppData\\Roaming\\itch";
    f.file(EPIC, "AAA.item", r#"{"DisplayName":"Rocket League","InstallLocation":"G:\\epic games\\rocketleague","AppCategories":["public","games","applications"]}"#)
        .file(EPIC, "BBB.item", r#"{"DisplayName":"Unreal Engine","InstallLocation":"G:\\epic games\\UE_5.4","AppCategories":["engines"]}"#)
        .file(EPIC, "CCC.item", "{ not json")
        .file(EPIC, "notes.txt", r#"{"InstallLocation":"G:\\nope"}"#)
        .key("SOFTWARE\\WOW6432Node\\GOG.com\\Games", "1207658924", &[("path", "H:\\GOG Games\\Witcher 3")])
        .key("SOFTWARE\\WOW6432Node\\GOG.com\\Games", "broken", &[("exe", "x")])
        .key("SOFTWARE\\WOW6432Node\\Ubisoft\\Launcher\\Installs", "635", &[("InstallDir", "I:/Ubisoft Games/Far Cry 5/")])
        .key("SOFTWARE\\WOW6432Node\\Electronic Arts", "Apex", &[("Install Dir", "J:\\EA Games\\Apex\\")])
        .key("SOFTWARE\\WOW6432Node\\Electronic Arts", "EA Desktop", &[("Install Dir", "X:\\Program Files\\Electronic Arts\\EA Desktop")])
        .key(UNINSTALL, "Overwatch", &[("Publisher", "Blizzard Entertainment"), ("InstallLocation", "K:\\Blizzard\\Overwatch")])
        .key(UNINSTALL, "Battle.net", &[("Publisher", "Blizzard Entertainment"), ("InstallLocation", "K:\\Battle.net")])
        .key(UNINSTALL, "Notepad++", &[("Publisher", "Notepad++ Team"), ("InstallLocation", "X:\\Program Files\\Notepad++")])
        .key(UNINSTALL, "BadGame", &[("Publisher", "Blizzard Entertainment"), ("InstallLocation", "C:\\")])
        .receipt(&format!("{itch}\\apps"), "celeste")
        .receipt("Y:\\itch games", "some-jam-game")
        .file(itch, "preferences.json", r#"{"installLocations":{"a1":{"path":"Y:\\itch games"}}}"#)
}

fn exe_in(path: &str) -> bool {
    is_game_library_path_in(path, &collect_roots(&machine()))
}

#[test]
fn every_launcher_library_is_game_evidence() {
    for exe in [
        "G:\\Epic Games\\RocketLeague\\Binaries\\Win64\\RocketLeague.exe",
        "H:\\GOG Games\\Witcher 3\\bin\\x64\\witcher3.exe",
        "I:\\Ubisoft Games\\Far Cry 5\\bin\\FarCry5.exe",
        "J:\\EA Games\\Apex\\r5apex.exe",
        "K:\\Blizzard\\Overwatch\\_retail_\\Overwatch.exe",
        "X:\\Users\\me\\AppData\\Roaming\\itch\\apps\\celeste\\Celeste.exe",
        "Y:\\itch games\\some-jam-game\\Game.exe",
        "\\\\?\\G:\\epic games\\rocketleague\\x.exe",
    ] {
        assert!(exe_in(exe), "{exe}");
    }
    assert!(exe_in("Z:\\SteamLibrary\\steamapps\\common\\Game\\game.exe"), "Steam still counts");
}

#[test]
fn launchers_themselves_engines_and_broad_folders_are_not() {
    for exe in [
        "G:\\epic games\\UE_5.4\\Engine\\Binaries\\Win64\\UnrealEditor.exe",
        "X:\\Program Files\\Electronic Arts\\EA Desktop\\EADesktop.exe",
        "K:\\Battle.net\\Battle.net.exe",
        "X:\\Program Files\\Notepad++\\notepad++.exe",
        "C:\\Windows\\System32\\mstsc.exe",
        "G:\\nope\\x.exe",
        "G:\\epic games\\rocketleagueX\\x.exe",
        "H:\\GOG Games\\Witcher 3",
    ] {
        assert!(!exe_in(exe), "{exe}");
    }
}

#[test]
fn missing_launchers_and_malformed_data_read_as_nothing() {
    assert!(collect_roots(&Fixture::default()).is_empty());
    let mut broad = Fixture::default();
    broad.env.insert("ProgramFiles".into(), "C:\\Program Files".into());
    let broad = broad
        .key("SOFTWARE\\WOW6432Node\\GOG.com\\Games", "a", &[("path", "C:\\Program Files")])
        .key("SOFTWARE\\WOW6432Node\\GOG.com\\Games", "b", &[("path", "D:\\")])
        .key("SOFTWARE\\WOW6432Node\\GOG.com\\Games", "c", &[("path", "relative")])
        .key("SOFTWARE\\WOW6432Node\\GOG.com\\Games", "d", &[("path", "  ")]);
    assert!(collect_roots(&broad).is_empty(), "{:?}", collect_roots(&broad));
    assert_eq!(epic_install("garbage"), None);
    assert_eq!(epic_install(r#"{"InstallLocation":"D:\\Games\\X"}"#).as_deref(), Some("D:\\Games\\X"), "no categories: kept");
    assert!(itch_locations("[1,2]").is_empty());
}

/// A plain (windowed) window of an exe at `path`, `steam_game` and
/// `launcher_game` as the watcher sets them.
fn event_at(path: &str, stem: &str, roots: &[String]) -> WatchEvent {
    WatchEvent {
        pid: 70,
        exe_stem: stem.into(),
        exe_name: format!("{stem}.exe"),
        title: stem.into(),
        fullscreen: false,
        fullscreen_on_target: false,
        steam_game: game_library_in(path, roots) == Library::Steam,
        launcher_game: game_library_in(path, roots) == Library::Launcher,
        store_game: false,
        hosted: false,
        window_class: String::new(),
        minecraft_java: false,
        hwnd: 0x700,
    }
}

#[test]
fn a_launcher_game_is_a_candidate_but_does_not_beat_the_private_list() {
    use crate::record_target_gate::adopted_candidate;
    use crate::record_target_rules::{classify_event, followable, game_evidence, skips_render_check};
    let roots = collect_roots(&machine());
    let cfg = Config::default();
    // An unknown exe in an Epic install, windowed: a candidate, no render check.
    let game = event_at("G:\\Epic Games\\RocketLeague\\Binaries\\Win64\\RocketLeague.exe", "RocketLeague", &roots);
    assert!(classify_event(&game, &cfg).is_some(), "foreground route");
    assert!(skips_render_check(&game, &cfg));
    assert!(adopted_candidate(&game, &cfg, (1280, 720)).is_some(), "adoption route");
    // R7-detect-1 (B): a private-list stem inside a GOG install is not a target.
    let chat = event_at("H:\\GOG Games\\Witcher 3\\Element.exe", "Element", &roots);
    assert!(!game_evidence(&chat), "launcher evidence does not beat the private list");
    assert!(classify_event(&chat, &cfg).is_none(), "foreground route");
    assert!(adopted_candidate(&chat, &cfg, (1280, 720)).is_none(), "adoption route");
    assert!(!followable(&chat, &cfg), "follow route");
    let fs = WatchEvent { fullscreen: true, fullscreen_on_target: true, ..chat.clone() };
    assert!(classify_event(&fs, &cfg).is_none(), "fullscreen");
    assert_eq!(crate::rules::classify(&crate::app::foreground_of(&fs), &cfg), crate::types::AppKind::NonGame, "HDR rules");
    // Steam still beats it, and an opt-in still records the GOG one.
    let steam = event_at("Z:\\SteamLibrary\\steamapps\\common\\Game\\Element.exe", "Element", &roots);
    assert!(classify_event(&steam, &cfg).is_some(), "Steam evidence");
    let mut listed = Config::default();
    listed.detect.fullscreen_games.push("Element".into());
    assert!(classify_event(&fs, &listed).is_some(), "fullscreen_games opt-in");
}

#[test]
fn itch_counts_only_folders_with_an_install_receipt() {
    let dl = "X:\\Users\\me\\Downloads";
    let prefs = r#"{"installLocations":{"a1":{"path":"Y:\\itch games"},"dl":{"path":"X:\\Users\\me\\Downloads"}}}"#;
    let f = machine().file(ITCH, "preferences.json", prefs).file(dl, "Telegram", "").receipt(dl, "jam");
    let f = f.file(&format!("{ITCH}\\apps"), "stray", "");
    let roots = collect_roots(&f);
    let cfg = Config::default();
    // A portable Telegram next to an itch install location in Downloads.
    let telegram = event_at("X:\\Users\\me\\Downloads\\Telegram\\Telegram.exe", "Telegram", &roots);
    assert!(!is_game_library_path_in("X:\\Users\\me\\Downloads\\Telegram\\Telegram.exe", &roots), "{roots:?}");
    assert!(crate::record_target::classify_event(&telegram, &cfg).is_none(), "not a target");
    let tool = event_at("X:\\Users\\me\\Downloads\\SomeTool\\tool.exe", "tool", &roots);
    assert!(crate::record_target::classify_event(&tool, &cfg).is_none(), "an unknown windowed exe in Downloads");
    assert!(!is_game_library_path_in(&format!("{ITCH}\\apps\\stray\\x.exe"), &roots), "apps\\stray: no receipt");
    for exe in ["X:\\Users\\me\\Downloads\\jam\\Jam.exe", "Y:\\itch games\\some-jam-game\\Game.exe", &format!("{ITCH}\\apps\\celeste\\Celeste.exe")] {
        assert!(is_game_library_path_in(exe, &roots), "{exe}: receipted install");
    }
}

#[test]
fn shared_and_profile_folders_are_never_a_library() {
    let mut f = machine();
    f.env.insert("ProgramFiles(x86)".into(), "X:\\Program Files (x86)".into());
    f.env.insert("LOCALAPPDATA".into(), "X:\\Users\\me\\AppData\\Local".into());
    let broad = [
        "X:\\Users\\me\\Downloads",
        "X:\\Users\\me\\Documents",
        "X:\\Users\\me\\Desktop",
        "X:\\Users\\me\\Videos",
        "X:\\Users\\me",
        "X:\\Users",
        "X:\\Users\\me\\AppData",
        "X:\\Users\\me\\AppData\\LocalLow",
        "X:\\Users\\me\\AppData\\Local\\Programs",
        "X:\\Users\\other\\Downloads",
        "X:\\Program Files",
        "X:\\Program Files (x86)",
        "D:\\",
    ];
    for (i, p) in broad.iter().enumerate() {
        f = f.key(GOG, &format!("b{i}"), &[("path", p)]);
    }
    let f = f.key(GOG, "foo", &[("path", "D:\\Games\\Foo")]);
    let roots = collect_roots(&f);
    for p in broad {
        assert!(!is_game_library_path_in(&format!("{p}\\Telegram\\Telegram.exe"), &roots), "{p}: too broad");
    }
    // A GOG game in D:\Games\Foo with that exact install path is game evidence.
    let foo = event_at("D:\\Games\\Foo\\Foo.exe", "Foo", &roots);
    assert!(crate::record_target::classify_event(&foo, &Config::default()).is_some(), "windowed GOG game");
    // A profile elsewhere: its ancestors and children are broad too.
    let mut moved = Fixture::default();
    moved.env.insert("USERPROFILE".into(), "D:\\Profiles\\me".into());
    let moved = moved.key(GOG, "a", &[("path", "D:\\Profiles")]).key(GOG, "b", &[("path", "D:\\Profiles\\me\\Downloads")]);
    assert!(collect_roots(&moved).is_empty(), "{:?}", collect_roots(&moved));
}

#[test]
fn prerequisite_installers_in_a_game_folder_are_not_evidence() {
    for exe in [
        "G:\\Epic Games\\RocketLeague\\Engine\\Extras\\Redist\\en-us\\UEPrereqSetup_x64.exe",
        "H:\\GOG Games\\Witcher 3\\__redist\\DirectX\\DXSETUP.exe",
        "J:\\EA Games\\Apex\\__Installer\\Cleanup.exe",
        "Z:\\SteamLibrary\\steamapps\\common\\Game\\_CommonRedist\\vcredist\\2019\\VC_redist.x64.exe",
        "Z:\\SteamLibrary\\steamapps\\common\\Game\\Redist\\setup.exe",
        "Z:\\SteamLibrary\\steamapps\\common\\Game\\DirectX\\DXSETUP.exe",
        "Z:\\SteamLibrary\\steamapps\\common\\Game\\vcredist\\vc.exe",
    ] {
        assert!(!exe_in(exe), "{exe}");
    }
    assert!(exe_in("Z:\\SteamLibrary\\steamapps\\common\\Redist Heroes\\game.exe"), "a whole segment, not a substring");
    assert!(exe_in("Z:\\SteamLibrary\\steamapps\\common\\Game\\redist.exe"), "the exe's own name is not a folder");
}

#[test]
fn the_cache_answers_lookups() {
    let roots = collect_roots(&machine());
    assert!(store_roots(roots.clone()));
    assert!(!store_roots(roots), "unchanged");
    assert!(is_launcher_library_path("H:\\GOG Games\\Witcher 3\\bin\\x64\\witcher3.exe"));
    assert!(!is_launcher_library_path("H:\\Elsewhere\\x.exe"));
}

#[test]
fn a_broad_gog_path_counts_only_for_the_exe_its_entry_names() {
    let f = machine()
        .key(GOG, "g1", &[("path", r"D:\Games"), ("exe", r"D:\Games\Foo\foo.exe")])
        .key(GOG, "g2", &[("path", r"E:\Library"), ("launchCommand", r#""E:\Library\bar.exe" -windowed"#)])
        .key(GOG, "g3", &[("path", r"F:\Shared\Stuff"), ("exeFile", r"baz\baz.exe")])
        .key(GOG, "g4", &[("path", r"F:\Shared\Stuff\Other")])
        .key(GOG, "g5", &[("path", r"M:\Parent")]);
    let roots = collect_roots(&f);
    for exe in [r"D:\Games\Foo\foo.exe", r"E:\Library\bar.exe", r"F:\Shared\Stuff\baz\baz.exe", r"F:\Shared\Stuff\Other\o.exe"] {
        assert!(is_game_library_path_in(exe, &roots), "{exe}: named by its entry: {roots:?}");
    }
    for exe in [
        r"D:\Games\Telegram\Telegram.exe",
        r"D:\Games\Foo\launcher.exe",
        r"E:\Library\tool.exe",
        r"F:\Shared\Stuff\chat\chat.exe",
        r"M:\Parent\x\x.exe",
    ] {
        assert!(!is_game_library_path_in(exe, &roots), "{exe}: under a broad GOG path: {roots:?}");
    }
    assert!(is_game_library_path_in(r"H:\GOG Games\Witcher 3\bin\x64\any.exe", &roots), "a one-game folder keeps every exe");
}

#[test]
fn an_epic_manifest_game_is_a_target_without_trace_rights() {
    let roots = collect_roots(&machine());
    let ev = event_at(r"G:\Epic Games\RocketLeague\Binaries\Win64\RocketLeague.exe", "RocketLeague", &roots);
    let fs = WatchEvent { fullscreen: true, fullscreen_on_target: true, ..ev };
    let c = crate::record_hosted::classify_checked(&fs, &Config::default(), false, false, || Some(false)).expect("Epic manifest is official proof");
    assert!(!c.needs_check);
    let tool = event_at(r"G:\Elsewhere\Tool\Tool.exe", "Tool", &roots);
    let tool = WatchEvent { fullscreen: true, fullscreen_on_target: true, ..tool };
    assert!(crate::record_hosted::classify_checked(&tool, &Config::default(), false, false, || Some(false)).is_none(), "outside every library: no proof");
}
