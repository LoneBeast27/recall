use super::*;
use crate::config::Config;
use crate::watcher::{is_game_library_path_in, WatchEvent};
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
        .file(&format!("{itch}\\apps"), "celeste", "")
        .file(itch, "preferences.json", r#"{"installLocations":{"a1":{"path":"L:\\itch games"}}}"#)
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
        "L:\\itch games\\some-jam-game\\Game.exe",
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

/// A plain (windowed) window of an exe at `path`, `steam_game` as the watcher sets it.
fn event_at(path: &str, stem: &str, roots: &[String]) -> WatchEvent {
    WatchEvent {
        pid: 70,
        exe_stem: stem.into(),
        exe_name: format!("{stem}.exe"),
        title: stem.into(),
        fullscreen: false,
        fullscreen_on_target: false,
        steam_game: is_game_library_path_in(path, roots),
        hosted: false,
        hwnd: 0x700,
    }
}

#[test]
fn a_launcher_game_counts_on_every_live_route() {
    let roots = collect_roots(&machine());
    let cfg = Config::default();
    // An Epic game called Element.exe (on the private list), windowed.
    let ev = event_at("G:\\Epic Games\\RocketLeague\\Element.exe", "Element", &roots);
    assert!(crate::record_target::game_evidence(&ev));
    assert!(crate::record_target::classify_event(&ev, &cfg).is_some(), "foreground route");
    assert!(crate::record_target_gate::adopted_candidate(&ev, &cfg, (1280, 720)).is_some(), "adoption route");
    assert!(crate::record_target::is_target_with(&ev.exe_stem, true, crate::record_target::game_evidence(&ev), &cfg), "follow route");
    let fs = WatchEvent { fullscreen: true, fullscreen_on_target: true, ..ev.clone() };
    assert_ne!(crate::rules::classify(&crate::app::foreground_of(&fs), &cfg), crate::types::AppKind::NonGame, "HDR rules");
    // The same exe outside any library stays private everywhere.
    let outside = event_at("D:\\Apps\\Element\\Element.exe", "Element", &roots);
    assert!(crate::record_target::classify_event(&outside, &cfg).is_none());
    assert!(crate::record_target_gate::adopted_candidate(&outside, &cfg, (1280, 720)).is_none());
}

#[test]
fn the_cache_answers_lookups() {
    let roots = collect_roots(&machine());
    assert!(store_roots(roots.clone()));
    assert!(!store_roots(roots), "unchanged");
    assert!(is_launcher_library_path("H:\\GOG Games\\Witcher 3\\bin\\x64\\witcher3.exe"));
    assert!(!is_launcher_library_path("H:\\Elsewhere\\x.exe"));
}
