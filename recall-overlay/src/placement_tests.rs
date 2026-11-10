//! `placement` tests.
use super::*;

/// A HUD grid column occupied over the rows [from, to) (multiples of 8).
fn rows(from: u32, to: u32) -> u64 {
    (from / 8..to / 8).fold(0, |bits, r| bits | 1 << r)
}

#[test]
fn the_island_keeps_masters_spots_for_every_game() {
    use Anchor::{TopCentre, TopLeft, TopRight};
    // The table on master: the re-scan feeds the hint card only (user's ruling, 2026-10-30).
    let master = [
        ("valorant", TopRight, 288),
        ("rocketleague", TopLeft, 48),
        ("apexlegends", TopCentre, 72),
        ("overwatch", TopCentre, 44),
        ("overwatch2", TopRight, 40),
        ("marvelrivals", TopRight, 56),
        ("genshinimpact", TopCentre, 104),
        ("battlefield6", TopRight, 0),
        ("counterstrike2", TopRight, 96),
        ("tomclancysrainbowsixsiege", TopRight, 376),
        ("rainbowsixsiege", TopRight, 376),
        ("counterstrikeglobaloffensive", TopRight, 64),
        ("forzahorizon6", TopCentre, 172),
        ("lethalcompany", TopCentre, 100),
    ];
    let spots = Spots::embedded();
    assert_eq!(spots.len(), master.len());
    for (key, anchor, free) in master {
        assert_eq!(spots.lookup(key), Spot { anchor, free_px: Some(free) }, "{key}");
        assert_eq!(spots.hud(key).map(|hud| hud.len()), Some(80), "{key} has its HUD grid for the hint card");
    }
}

#[test]
fn embedded_table_parses_every_scanned_game() {
    let spots = Spots::embedded();
    assert_eq!(spots.lookup("rocketleague64bitdx11cooked").anchor, Anchor::TopLeft, "title suffixes still match");
    assert_eq!(spots.lookup("somenewgame"), DEFAULT_SPOT);
    assert_eq!((spots.lookup(""), spots.hud("")), (DEFAULT_SPOT, None), "desktop uses top-right");
}

/// The controller's target name is the window title, not the scan's key:
/// each name here takes the path a status takes to `App::place`.
#[test]
fn real_target_names_reach_their_scanned_spots() {
    use crate::session::Session;
    let names = [
        // Seen as `record: target -> "…"` in recall-hdr.log.
        ("Rocket League (64-bit, DX11, Cooked)", "RocketLeague.exe", "rocketleague"),
        ("VALORANT", "VALORANT-Win64-Shipping.exe", "valorant"),
        ("Counter-Strike 2", "cs2.exe", "counterstrike2"),
        ("Genshin Impact", "GenshinImpact.exe", "genshinimpact"),
        // As the clip library names them.
        ("Rocket League®", "RocketLeague.exe", "rocketleague"),
        ("Apex Legends", "r5apex.exe", "apexlegends"),
        ("Overwatch", "Overwatch.exe", "overwatch"),
        ("Overwatch® 2", "Overwatch.exe", "overwatch2"),
        ("Marvel Rivals", "Marvel-Win64-Shipping.exe", "marvelrivals"),
        ("GenshinImpact", "GenshinImpact.exe", "genshinimpact"),
        ("Battlefield™ 6", "bf6.exe", "battlefield6"),
        ("Tom Clancys Rainbow Six Siege", "RainbowSix.exe", "tomclancysrainbowsixsiege"),
        ("Counter-Strike Global Offensive", "csgo.exe", "counterstrikeglobaloffensive"),
        ("Forza Horizon 6", "forza_gaming.desktop.x64_release_final.exe", "forzahorizon6"),
        ("Lethal Company", "Lethal Company.exe", "lethalcompany"),
    ];
    let spots = Spots::embedded();
    for (name, exe, scanned) in names {
        let mut session = Session::default();
        session.apply_status(&serde_json::json!({"type":"status","state":"recording","target":{"name":name,"exe":exe,"pid":42}}));
        assert_eq!(spots.lookup(&session.game_key()), spots.lookup(scanned), "{name}");
        assert_ne!(spots.lookup(&session.game_key()), DEFAULT_SPOT, "{name} is not the default spot");
    }
    let monitor = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    let mut rocket = Session::default();
    rocket.apply_status(&serde_json::json!({"type":"status","state":"recording","target":{"name":names[0].0,"exe":names[0].1,"pid":42}}));
    assert_eq!(position(spots.lookup(&rocket.game_key()).anchor, &monitor, 300, 12), (12, 12), "Rocket League sits top-left");

    // An empty window title sends the exe stem as the name (`display_name` in recall-hdr).
    let builds = [("r5apex_dx12.exe", "apexlegends"), ("RainbowSix_Vulkan.exe", "tomclancysrainbowsixsiege")];
    let stems = names.iter().map(|&(_, exe, scanned)| (exe, scanned)).chain(builds);
    let mut misses = Vec::new();
    for (exe, scanned) in stems {
        // Overwatch.exe runs both games: its stem cannot tell them apart.
        let scanned = if scanned == "overwatch2" { "overwatch" } else { scanned };
        let stem = exe.strip_suffix(".exe").unwrap();
        let mut session = Session::default();
        session.apply_status(&serde_json::json!({"type":"status","state":"recording","target":{"name":stem,"exe":exe,"pid":42}}));
        let key = session.game_key();
        if spots.lookup(&key) != spots.lookup(scanned) || spots.hud(&key) != spots.hud(scanned) {
            misses.push(stem);
        }
    }
    assert!(misses.is_empty(), "exe stems that miss their scanned spot: {misses:?}");
}

#[test]
fn scale_follows_monitor_height() {
    let screen = |w: i32, h: i32| Rect { left: 0, top: 0, right: w, bottom: h };
    assert_eq!(scale(&screen(2560, 1440), &DEFAULT_SPOT), 1.0);
    assert_eq!(scale(&screen(3840, 2160), &DEFAULT_SPOT), 1.5);
    assert_eq!(scale(&screen(1920, 1080), &DEFAULT_SPOT), 0.75);
    let roomy = Spot { anchor: Anchor::TopRight, free_px: Some(288) };
    assert_eq!(scale(&screen(2560, 1440), &roomy), 1.0, "plenty of room: full size");
}

#[test]
fn a_portrait_monitor_scales_by_its_short_side() {
    let portrait = Rect { left: 0, top: 0, right: 1440, bottom: 2560 };
    assert_eq!(scale(&portrait, &DEFAULT_SPOT), 1.0, "not 1.78");
    assert_eq!(scale(&Rect { left: -1080, top: 0, right: 0, bottom: 1920 }, &DEFAULT_SPOT), 0.75);
}

#[test]
fn tight_huds_shrink_the_island_to_fit_but_not_below_the_floor() {
    // Overwatch 2: 40 free rows < 12 + 44 → the island's bottom lands on row 40.
    let tight = Spot { anchor: Anchor::TopRight, free_px: Some(40) };
    let k = scale(&Rect { left: 0, top: 0, right: 2560, bottom: 1440 }, &tight);
    assert!(((EDGE + ISLAND_H) * k - 40.0).abs() < 1e-3);
    assert_eq!(fit(Some(56)), 1.0);
    assert_eq!(fit(Some(10)), MIN_FIT);
    assert_eq!(fit(Some(0)), 1.0, "no free spot at all: keep full size (the scan says it overlaps)");
    assert_eq!(fit(None), 1.0);
}

#[test]
fn action_notices_grow_only_into_the_free_depth() {
    let full = NOTICE_H / ISLAND_H;
    assert_eq!(grow(&DEFAULT_SPOT), full, "unscanned: full growth");
    let roomy = Spot { anchor: Anchor::TopRight, free_px: Some(288) };
    assert_eq!(grow(&roomy), full, "288 free rows");
    for free in [48, 44, 40, 56] {
        let tight = Spot { anchor: Anchor::TopRight, free_px: Some(free) };
        assert_eq!(grow(&tight), 1.0, "{free} free rows: no room below, sideways only");
        let k = fit(Some(free));
        assert!((EDGE + ISLAND_H) * k <= free as f32 + 1e-3);
    }
    let csgo = Spot { anchor: Anchor::TopRight, free_px: Some(64) };
    let g = grow(&csgo);
    assert!(g > 1.0 && g < full, "partial growth: {g}");
    assert!(EDGE + ISLAND_H * g * OVERSHOOT_H <= 64.0 + 1e-3, "peak stays above the HUD");
}

#[test]
fn free_depth_is_exactly_the_tight_island_and_unbounded_when_unscanned() {
    for height in [1080, 1440, 2160] {
        let monitor = Rect { left: 0, top: 0, right: height * 16 / 9, bottom: height };
        let tight = Spot { anchor: Anchor::TopRight, free_px: Some(40) };
        let k = scale(&Rect { left: 0, top: 0, right: height * 16 / 9, bottom: height }, &tight);
        assert!((free_depth(&tight, &monitor, k) - ISLAND_H * k).abs() < 1e-3, "the fitted island fills it at {height}p");
        assert_eq!(free_depth(&DEFAULT_SPOT, &monitor, 1.0), f32::INFINITY);
        let none_free = Spot { anchor: Anchor::TopRight, free_px: Some(0) };
        assert_eq!(free_depth(&none_free, &monitor, 1.0), f32::INFINITY, "no free spot: nothing to protect");
    }
}

#[test]
fn a_card_covers_the_hud_under_its_own_columns() {
    // VALORANT-like: HUD from the top as far as x 2080, then the kill feed from row 120.
    let hud: Vec<u64> = (0..80).map(|i| if i < 65 { rows(0, 512) } else { rows(120, 512) }).collect();
    let spot = Spot { anchor: Anchor::TopRight, free_px: Some(288) };
    let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    let cover = |w: f32, h: f32| card_cover(&spot, Some(&hud), &qhd, 1.0, (w, h), 50.0);
    assert_eq!(cover(172.0, 52.0), Some(0.0), "clear above the kill feed (rows 62-114)");
    assert_eq!(cover(299.0, 116.0), Some(58.0 * 299.0), "rows 120-178 over the kill feed");
    assert_eq!(cover(636.0, 52.0), Some(168.0 * 52.0), "its 168 px left of x 2080, all 52 rows");
    let uhd = Rect { left: 0, top: 0, right: 3840, bottom: 2160 };
    assert_eq!(card_cover(&spot, Some(&hud), &uhd, 1.5, (448.5, 174.0), 75.0), Some(58.0 * 299.0), "4K: the same 1440p area");
    let left = Spot { anchor: Anchor::TopLeft, ..spot };
    let wall: Vec<u64> = (0..80).map(|i| if i < 5 { u64::MAX } else { 0 }).collect();
    assert_eq!(card_cover(&left, Some(&wall), &qhd, 1.0, (200.0, 52.0), 50.0), Some(148.0 * 52.0), "from the left edge: x 12-160");
    let centre = Spot { anchor: Anchor::TopCentre, ..spot };
    assert_eq!(card_cover(&centre, Some(&wall), &qhd, 1.0, (200.0, 52.0), 50.0), Some(0.0), "centred, far from the wall");
}

#[test]
fn only_the_rows_that_hold_hud_count_not_everything_below_them() {
    // A thin banner across the screen, rows 96-112, and nothing under it.
    let hud = vec![rows(96, 112); 80];
    let spot = Spot { anchor: Anchor::TopRight, free_px: Some(288) };
    let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    assert_eq!(card_cover(&spot, Some(&hud), &qhd, 1.0, (172.0, 180.0), 50.0), Some(16.0 * 172.0), "16 rows of the card's 180");
}

#[test]
fn without_a_grid_the_one_free_depth_stands_in_and_unscanned_covers_nothing_known() {
    let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    let flat = Spot { anchor: Anchor::TopRight, free_px: Some(200) };
    assert_eq!(card_cover(&flat, None, &qhd, 1.0, (172.0, 180.0), 50.0), Some(42.0 * 172.0), "rows 200-242");
    assert_eq!(card_cover(&flat, None, &qhd, 1.0, (299.0, 116.0), 50.0), Some(0.0));
    assert_eq!(card_cover(&DEFAULT_SPOT, None, &qhd, 1.0, (172.0, 180.0), 50.0), None, "no scan data");
    let none_free = Spot { anchor: Anchor::TopRight, free_px: Some(0) };
    assert_eq!(card_cover(&none_free, None, &qhd, 1.0, (172.0, 180.0), 50.0), None);
}

#[test]
fn spots_parse_their_hud_grids() {
    let hud: Vec<u64> = vec![u64::MAX; 80];
    let json = format!(r#"{{"spots":{{"game":{{"anchor":"top-right","free_px":124,"hud":{hud:?}}},"old":{{"anchor":"top-left","free_px":48}}}}}}"#);
    let spots = Spots::parse(&json).unwrap();
    assert_eq!((spots.hud("game"), spots.hud("game64bit").is_some()), (Some(hud), true), "suffixes match too");
    assert_eq!(spots.hud("old"), None, "a spot without a grid");
}

#[test]
fn a_window_with_a_margin_keeps_its_box_on_the_anchor() {
    let monitor = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    assert_eq!(window_origin(Anchor::TopRight, &monitor, 44.0, 0.0, 12), (2504, 12), "the circle sits at the right end");
    assert_eq!(window_origin(Anchor::TopRight, &monitor, 44.0, 2.0, 12), (2502, 10), "the margin wraps the same box");
    assert_eq!(window_origin(Anchor::TopRight, &monitor, 299.4, 0.0, 12), position(Anchor::TopRight, &monitor, 300, 12));
}

#[test]
fn bottom_and_side_spots_keep_the_box_on_their_edge() {
    let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    // A 300 x 44 pill with a 100 px card unfolded: 300 along, 144 deep.
    assert_eq!(look_origin(Anchor::BottomRight, &qhd, 300.0, 144.0, 0.0, 12), (2248, 1440 - 12 - 144), "grows upwards");
    assert_eq!(look_origin(Anchor::BottomLeft, &qhd, 300.0, 44.0, 2.0, 12), (10, 1440 - 12 - 44 - 2));
    assert_eq!(look_origin(Anchor::BottomCentre, &qhd, 300.0, 44.0, 0.0, 12), (1130, 1384));
    // Side spots: the turned window is `depth` wide and `along` high, centred down the edge.
    assert_eq!(look_origin(Anchor::RightMiddle, &qhd, 300.0, 44.0, 0.0, 12), (2560 - 12 - 44, 570));
    assert_eq!(look_origin(Anchor::LeftMiddle, &qhd, 300.0, 144.0, 0.0, 12), (12, 570), "the card reaches inwards");
    let left_screen = Rect { left: -1920, top: -200, right: 0, bottom: 880 };
    assert_eq!(look_origin(Anchor::RightMiddle, &left_screen, 200.0, 44.0, 0.0, 9), (-53, 240));
}

#[test]
fn corners_turns_and_alignment_per_anchor() {
    use crate::presence::Turn;
    assert_eq!(Anchor::from_corner("bottom-left"), Some(Anchor::BottomLeft));
    assert_eq!(Anchor::from_corner("top-right"), Some(Anchor::TopRight));
    assert_eq!(Anchor::from_corner("dynamic"), None);
    assert_eq!((Anchor::RightMiddle.turn(), Anchor::LeftMiddle.turn(), Anchor::BottomRight.turn()), (Turn::Right, Turn::Left, Turn::None));
    assert_eq!((align(Anchor::BottomLeft), align(Anchor::RightMiddle), align(Anchor::BottomRight)), (0.0, 0.5, 1.0));
    let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    let low = Spot { anchor: Anchor::BottomRight, free_px: None };
    assert_eq!(card_cover(&low, Some(&[u64::MAX; 80]), &qhd, 1.0, (200.0, 52.0), 50.0), None, "the scan's grid is the top rows only");
}

#[test]
fn anchors_place_the_island_inside_the_monitor() {
    let monitor = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    assert_eq!(position(Anchor::TopRight, &monitor, 300, 12), (2248, 12));
    assert_eq!(position(Anchor::TopLeft, &monitor, 300, 12), (12, 12));
    assert_eq!(position(Anchor::TopCentre, &monitor, 300, 12), (1130, 12));
    let left_screen = Rect { left: -1920, top: -200, right: 0, bottom: 880 };
    assert_eq!(position(Anchor::TopRight, &left_screen, 200, 9), (-209, -191));
}
