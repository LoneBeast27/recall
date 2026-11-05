use super::*;

fn profile(marker: Option<u32>) -> Lookup {
    Lookup::Profile { name: "x".into(), marker }
}

#[test]
fn only_a_nonzero_marker_is_a_game() {
    assert!(profile(Some(4)).is_game(), "VALORANT, Rocket League, CS2: 4");
    assert!(profile(Some(1)).is_game(), "Portal 2, Gang Beasts: 1");
    assert!(!profile(Some(0)).is_game(), "Wallpaper Engine, Fortnite: 0");
    assert!(!profile(None).is_game(), "Chrome, Discord, VLC: no marker");
    assert!(!Lookup::NoProfile.is_game());
}

#[test]
fn version_words_match_the_c_header_sizes() {
    // MAKE_NVAPI_VERSION(struct, ver) = sizeof | ver << 16 (nvapi.h).
    assert_eq!(drs::app_version() & 0xFFFF, 20_492, "NVDRS_APPLICATION_V4");
    assert_eq!(drs::app_version() >> 16, 4);
    assert_eq!(drs::profile_version() & 0xFFFF, 4_116, "NVDRS_PROFILE_V1");
    assert_eq!(drs::setting_version() & 0xFFFF, 12_320, "NVDRS_SETTING_V1");
}

/// Live: queries this PC's driver database by exe name. Opens no app.
/// `cargo test -p recall-nvidia live -- --ignored --nocapture`
#[test]
#[ignore]
fn live_lookup_by_name() {
    let t = std::time::Instant::now();
    let nv = Nvapi::load().expect("nvapi64.dll (NVIDIA driver)");
    println!("load + initialize: {:?}", t.elapsed());
    let apps = ["VALORANT-Win64-Shipping.exe", "RocketLeague.exe", "cs2.exe", "chrome.exe", "msedge.exe", "Discord.exe", "vlc.exe", "Spotify.exe", "POWERPNT.EXE", "notepad.exe", "no-such-app-xyz.exe"];
    let t = std::time::Instant::now();
    let found = nv.lookup(&apps).expect("DRS lookup");
    println!("session + load settings + {} lookups: {:?}", apps.len(), t.elapsed());
    for (a, l) in apps.iter().zip(&found) {
        println!("{a:32} {l:?} game={}", l.is_game());
    }
    let game = |a: &str| found[apps.iter().position(|x| *x == a).unwrap()].is_game();
    for g in ["VALORANT-Win64-Shipping.exe", "RocketLeague.exe", "cs2.exe"] {
        assert!(game(g), "{g}");
    }
    for n in ["chrome.exe", "msedge.exe", "Discord.exe", "vlc.exe", "Spotify.exe", "POWERPNT.EXE", "notepad.exe", "no-such-app-xyz.exe"] {
        assert!(!game(n), "{n}");
    }
}
