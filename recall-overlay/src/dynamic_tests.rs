use super::*;
use serde_json::json;

/// A 64x36 grid with HUD over the picture fraction `[x0, x1) x [y0, y1)`.
fn hud(boxes: &[(f32, f32, f32, f32)]) -> HudGrid {
    let (cols, rows) = (64u32, 36u32);
    let grid = (0..rows)
        .map(|r| {
            (0..cols).fold(0u64, |bits, c| {
                let (x, y) = ((c as f32 + 0.5) / cols as f32, (r as f32 + 0.5) / rows as f32);
                let hit = boxes.iter().any(|&(x0, y0, x1, y1)| x >= x0 && x < x1 && y >= y0 && y < y1);
                bits | (hit as u64) << c
            })
        })
        .collect();
    HudGrid { cols, rows, grid }
}

const WIDE: f32 = 16.0 / 9.0;

#[test]
fn no_hud_keeps_the_top_right() {
    assert_eq!(choose(&hud(&[]), WIDE, None), Anchor::TopRight);
}

#[test]
fn hud_in_a_corner_moves_the_island_to_the_next_free_one() {
    // A kill feed top right.
    let feed = hud(&[(0.75, 0.0, 1.0, 0.25)]);
    assert_eq!(choose(&feed, WIDE, None), Anchor::TopLeft);
    // Both top corners taken (a minimap and a kill feed): the bottom right.
    let busy = hud(&[(0.75, 0.0, 1.0, 0.25), (0.0, 0.0, 0.2, 0.3)]);
    assert_eq!(choose(&busy, WIDE, None), Anchor::BottomRight);
}

#[test]
fn every_corner_taken_puts_the_island_on_a_side_edge() {
    let corners = [(0.0, 0.0, 0.25, 0.2), (0.75, 0.0, 1.0, 0.2), (0.0, 0.8, 0.25, 1.0), (0.75, 0.8, 1.0, 1.0)];
    assert_eq!(choose(&hud(&corners), WIDE, None), Anchor::RightMiddle);
    let mut right = corners.to_vec();
    right.push((0.9, 0.3, 1.0, 0.7)); // an ability bar down the right side
    assert_eq!(choose(&hud(&right), WIDE, None), Anchor::LeftMiddle);
}

#[test]
fn the_least_covered_spot_wins_when_none_is_clear() {
    // Everything along the edges is covered, the top-left only a little.
    let mut boxes = vec![(0.0, 0.0, 1.0, 0.06), (0.0, 0.94, 1.0, 1.0), (0.0, 0.0, 0.04, 1.0), (0.96, 0.0, 1.0, 1.0)];
    boxes.push((0.12, 0.0, 0.88, 0.2));
    let g = hud(&boxes);
    let top_left = g.cover(footprint(Anchor::TopLeft, WIDE));
    for a in PREFERENCE {
        assert!(g.cover(footprint(a, WIDE)) >= top_left.min(g.cover(footprint(choose(&g, WIDE, None), WIDE))));
    }
    let picked = choose(&g, WIDE, None);
    assert_eq!(g.cover(footprint(picked, WIDE)), PREFERENCE.iter().map(|&a| g.cover(footprint(a, WIDE))).min().unwrap());
}

#[test]
fn an_equally_clear_current_spot_is_kept() {
    let g = hud(&[(0.75, 0.0, 1.0, 0.25)]);
    assert_eq!(choose(&g, WIDE, Some(Anchor::BottomLeft)), Anchor::BottomLeft, "no hop from one free spot to another");
    assert_eq!(choose(&g, WIDE, Some(Anchor::TopRight)), Anchor::TopLeft, "a covered spot is left");
}

#[test]
fn footprints_sit_on_their_edges() {
    for a in PREFERENCE {
        let (x0, y0, x1, y1) = footprint(a, WIDE);
        assert!(0.0 <= x0 && x0 < x1 && x1 <= 1.0 && 0.0 <= y0 && y0 < y1 && y1 <= 1.0, "{a:?}");
    }
    let (_, y0, _, y1) = footprint(Anchor::RightMiddle, WIDE);
    assert!((y1 - y0 - 560.0 / 1440.0).abs() < 1e-4, "a side spot runs along the edge, tall (stacked glyphs)");
    let (x0, _, x1, _) = footprint(Anchor::TopLeft, 21.0 / 9.0);
    assert!(x1 - x0 < 360.0 / 2560.0, "ultrawide: the same pill is a smaller share of the width");
}

#[test]
fn scans_parse_only_when_ok_and_well_formed() {
    let ok = json!({"type":"hud_scan","ok":true,"cols":2,"rows":2,"grid":[1,2],"moving":8,"bursts":9});
    assert_eq!(HudGrid::from_scan(&ok), Some(HudGrid { cols: 2, rows: 2, grid: vec![1, 2] }));
    assert_eq!(HudGrid::from_scan(&json!({"ok":false,"cols":2,"rows":2,"grid":[],"reason":"menus"})), None);
    assert_eq!(HudGrid::from_scan(&json!({"ok":true,"cols":2,"rows":3,"grid":[1,2]})), None, "rows mismatch");
    assert_eq!(HudGrid::from_scan(&json!({"ok":true,"cols":65,"rows":1,"grid":[1]})), None, "64 columns at most");
}

#[test]
fn the_cache_keeps_the_newest_scans_and_reads_them_together() {
    let mut cache = HudCache::load(None);
    assert_eq!((cache.scans("rocketleague"), cache.grid("rocketleague")), (0, None));
    cache.add("rocketleague", HudGrid { cols: 4, rows: 2, grid: vec![0b0001, 0] });
    cache.add("rocketleague", HudGrid { cols: 4, rows: 2, grid: vec![0, 0b1000] });
    assert_eq!(cache.grid("rocketleague"), Some(HudGrid { cols: 4, rows: 2, grid: vec![0b0001, 0b1000] }), "HUD seen in any scan");
    for _ in 0..KEEP {
        cache.add("rocketleague", HudGrid { cols: 4, rows: 2, grid: vec![0, 0] });
    }
    assert_eq!(cache.scans("rocketleague"), KEEP);
    assert_eq!(cache.grid("rocketleague").unwrap().grid, vec![0, 0], "the oldest scans roll off");
    cache.add("rocketleague", HudGrid { cols: 8, rows: 2, grid: vec![1, 1] });
    assert_eq!(cache.scans("rocketleague"), 1, "another grid size starts over");
    cache.add("", HudGrid { cols: 4, rows: 2, grid: vec![1, 1] });
    assert_eq!(cache.scans(""), 0, "the desktop is never cached");
}

#[test]
fn the_cache_survives_a_restart_on_disk() {
    let dir = std::env::temp_dir().join(format!("recall-island-hud-{}", std::process::id()));
    let path = dir.join("island-hud.json");
    let mut cache = HudCache::load(Some(path.clone()));
    cache.add("valorant", HudGrid { cols: 8, rows: 1, grid: vec![2] });
    let back = HudCache::load(Some(path.clone()));
    assert_eq!(back.grid("valorant"), Some(HudGrid { cols: 8, rows: 1, grid: vec![2] }));
    assert!(!path.with_extension("json.tmp").exists(), "the temp file was renamed into place");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_new_game_is_probed_soon_and_then_ever_more_rarely() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let mut p = Prober::default();
    assert!(!p.due("valorant", true, 0, t0));
    assert!(!p.due("valorant", true, 0, at(19)));
    assert!(p.due("valorant", true, 0, at(20)), "20 s in");
    assert!(!p.due("valorant", true, 0, at(30)), "one probe at a time");
    assert_eq!(p.answered(true, 1, at(60)), Some("valorant".into()));
    assert!(!p.due("valorant", true, 1, at(60 + 179)));
    assert!(p.due("valorant", true, 1, at(60 + 180)), "3 min later while unsettled");
    assert_eq!(p.answered(true, SETTLED, at(300)), Some("valorant".into()));
    assert!(!p.due("valorant", true, SETTLED, at(300 + 29 * 60)));
    assert!(p.due("valorant", true, SETTLED, at(300 + 30 * 60)), "30 min once settled");
}

#[test]
fn a_settled_game_waits_ten_minutes_and_no_game_or_no_focus_waits_forever() {
    let t0 = Instant::now();
    let mut p = Prober::default();
    assert!(!p.due("valorant", true, SETTLED, t0));
    assert!(!p.due("valorant", true, SETTLED, t0 + Duration::from_secs(599)));
    assert!(!p.due("valorant", false, SETTLED, t0 + Duration::from_secs(700)), "not in front");
    assert!(p.due("valorant", true, SETTLED, t0 + Duration::from_secs(701)));
    let mut idle = Prober::default();
    assert!(!idle.due("", true, 0, t0 + Duration::from_secs(3600)), "the desktop is never probed");
}

#[test]
fn failures_retry_a_few_times_then_stop_and_a_new_game_starts_fresh() {
    let t0 = Instant::now();
    let mut now = t0 + Duration::from_secs(20);
    let mut p = Prober::default();
    p.due("rocketleague", true, 0, t0);
    for _ in 0..MAX_FAILURES {
        assert!(p.due("rocketleague", true, 0, now));
        assert_eq!(p.answered(false, 0, now + Duration::from_secs(60)), Some("rocketleague".into()));
        now += Duration::from_secs(60) + RETRY;
    }
    assert!(!p.due("rocketleague", true, 0, now + Duration::from_secs(3600)), "gave up for this session");
    assert!(!p.due("valorant", true, 0, now), "a new game");
    assert!(p.due("valorant", true, 0, now + FIRST_UNSCANNED));
    // A probe that never answers counts as failed.
    let later = now + FIRST_UNSCANNED + PENDING_FOR;
    assert!(!p.due("valorant", true, 0, later), "timed out: retry later");
    assert!(p.due("valorant", true, 0, later + RETRY));
}

#[test]
fn an_answer_for_a_game_that_left_is_dropped() {
    let t0 = Instant::now();
    let mut p = Prober::default();
    p.due("valorant", true, 0, t0);
    assert!(p.due("valorant", true, 0, t0 + FIRST_UNSCANNED));
    p.due("rocketleague", true, 0, t0 + Duration::from_secs(30));
    assert_eq!(p.answered(true, 1, t0 + Duration::from_secs(40)), None);
    assert_eq!(Prober::default().answered(true, 1, t0), None, "nothing was asked");
}

#[test]
fn a_bad_cache_file_is_cleaned_on_load_and_the_game_count_is_capped() {
    let dir = std::env::temp_dir().join(format!("recall-island-hud-bad-{}", std::process::id()));
    let path = dir.join("island-hud.json");
    std::fs::create_dir_all(&dir).unwrap();
    let bad = r#"{"games":{"huge":{"cols":1000000,"rows":2,"scans":[[1,2]]},"short":{"cols":4,"rows":3,"scans":[[1]]},"fine":{"cols":4,"rows":1,"scans":[[1]]}}}"#;
    std::fs::write(&path, bad).unwrap();
    let cache = HudCache::load(Some(path));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!((cache.scans("huge"), cache.scans("short"), cache.scans("fine")), (0, 0, 1));
    let mut many = HudCache::load(None);
    for i in 0..MAX_GAMES + 5 {
        many.add(&format!("game{i}"), HudGrid { cols: 8, rows: 1, grid: vec![1] });
    }
    many.add("game0", HudGrid { cols: 8, rows: 1, grid: vec![1] });
    assert_eq!(many.file.games.len(), MAX_GAMES);
    assert_eq!(many.scans("game1"), 0, "the game scanned longest ago went first");
    assert_eq!(many.scans("game0"), 1, "dropped earlier, a rescan brings it back fresh");
    assert_eq!(many.scans("game5"), 0, "and pushes out the next oldest");
    assert_eq!(many.scans(&format!("game{}", MAX_GAMES + 4)), 1);
}

#[test]
fn scans_that_mark_most_of_the_screen_are_ignored() {
    let mut cache = HudCache::load(None);
    let menu = hud(&[(0.0, 0.0, 1.0, 0.7)]);
    cache.add("rocketleague", menu.clone());
    assert_eq!(cache.scans("rocketleague"), 0, "a new menu scan is not kept");
    // Saved before this check: skipped when read.
    cache.file.games.insert("rocketleague".into(), Entry { cols: 64, rows: 36, scans: vec![menu.grid.clone()], seq: 1 });
    assert_eq!(cache.grid("rocketleague"), None);
    let feed = hud(&[(0.4, 0.0, 0.6, 0.08)]);
    cache.add("rocketleague", feed.clone());
    assert_eq!(cache.grid("rocketleague"), Some(feed), "only the good scan counts");
    assert_eq!(choose(&cache.grid("rocketleague").unwrap(), WIDE, None), Anchor::TopRight);
}

/// Rohan's Rocket League scans from 2026-11-06: two good ones (score top centre,
/// boost bottom right) and two taken in menus that marked ~65% of the screen.
#[test]
fn rocket_league_lands_top_right_once_menu_scans_are_skipped() {
    let rows: Vec<(u32, u32, u32)> = vec![(0, 27, 37), (1, 27, 37), (2, 27, 37), (3, 53, 58), (4, 53, 62), (34, 56, 64), (35, 56, 63), (30, 56, 62)];
    let mut good = vec![0u64; 36];
    for (r, c0, c1) in rows {
        good[r as usize] |= (c0..c1).fold(0u64, |b, c| b | 1 << c);
    }
    let menu: Vec<u64> = (0..36).map(|r| if r % 3 == 2 { 0 } else { u64::MAX }).collect();
    let mut cache = HudCache::load(None);
    cache.file.games.insert("rocketleague".into(), Entry { cols: 64, rows: 36, scans: vec![good.clone(), menu.clone(), menu, good], seq: 4 });
    let grid = cache.grid("rocketleague").unwrap();
    assert_eq!(choose(&grid, WIDE, None), Anchor::TopRight);
}

#[test]
fn title_like_keys_are_dropped_on_load_and_the_file_is_rewritten() {
    let dir = std::env::temp_dir().join(format!("recall-island-hud-title-{}", std::process::id()));
    let path = dir.join("island-hud.json");
    std::fs::create_dir_all(&dir).unwrap();
    let file = r#"{"games":{"My Secret Chat - Discord":{"cols":4,"rows":1,"scans":[[1]]},"Rocket League (64-bit)":{"cols":4,"rows":1,"scans":[[1]]},"rocketleague":{"cols":4,"rows":1,"scans":[[1]]},"valorant-win64.shipping_x":{"cols":4,"rows":1,"scans":[[1]]}}}"#;
    std::fs::write(&path, file).unwrap();
    let cache = HudCache::load(Some(path.clone()));
    assert_eq!(cache.file.games.keys().map(String::as_str).collect::<Vec<_>>(), ["rocketleague", "valorant-win64.shipping_x"]);
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(!on_disk.contains("Secret") && !on_disk.contains("Rocket League"), "the file was rewritten without titles: {on_disk}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exe_stem_keys_are_plain_lowercase_names() {
    assert!(exe_stem_key("rocketleague") && exe_stem_key("valorant-win64-shipping") && exe_stem_key("cs2.x_y"));
    assert!(!exe_stem_key("") && !exe_stem_key("Rocket League") && !exe_stem_key("Discord") && !exe_stem_key("a b"));
    assert!(!exe_stem_key(&"a".repeat(65)) && exe_stem_key(&"a".repeat(64)));
}
