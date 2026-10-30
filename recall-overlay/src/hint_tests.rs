use super::*;
use crate::layout::TextMetrics;

/// Every char 0.6 em wide (as `layout_tests`' fake).
struct Fake;

impl Measure for Fake {
    fn measure(&self, role: Role, scale: f32, text: &str) -> TextMetrics {
        let em = role.size() * scale;
        TextMetrics { width: text.chars().count() as f32 * em * 0.6, height: em * 1.25, baseline: em * 0.95, cap: em * 0.7 }
    }
}

fn hint() -> HintView {
    let option = |key: &str, label: &str| HintOption { key: key.into(), label: label.into() };
    HintView {
        leader: "Alt `".into(),
        options: vec![
            option("1", "Bookmark"),
            option("2", "Save 30 s"),
            option("3", "Screenshot"),
            option("4", "Record"),
            option("5", "Performance stats"),
        ],
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn heights_at_1440p_drop_down_180_two_columns_116_one_row_52() {
    assert_eq!(CardLayout::Dropdown.height(5, 1.0), 180.0);
    assert_eq!(CardLayout::TwoColumn.height(5, 1.0), 116.0);
    assert_eq!(CardLayout::Row.height(5, 1.0), 52.0);
    assert_eq!(CardLayout::Dropdown.height(5, 1.5), 270.0, "scales with the island");
}

/// What `choose_card` picks when the cards (by height) cover these areas.
fn picked(drop: f32, two: f32, row: f32) -> CardLayout {
    let cover = |_: f32, h: f32| Some(if h >= 180.0 { drop } else if h >= 116.0 { two } else { row });
    choose_card(&hint(), 1.0, &Fake, cover).layout
}

#[test]
fn a_card_that_covers_nothing_wins_in_preference_order() {
    assert_eq!(picked(0.0, 0.0, 0.0), CardLayout::Dropdown, "all clean: the drop-down");
    assert_eq!(picked(500.0, 0.0, 0.0), CardLayout::TwoColumn, "the first clean one");
    assert_eq!(picked(500.0, 900.0, 0.0), CardLayout::Row);
}

#[test]
fn when_none_is_clean_the_least_covered_area_wins_whatever_its_layout() {
    assert_eq!(picked(100.0, 200.0, 300.0), CardLayout::Dropdown, "not the row just because it is shortest");
    assert_eq!(picked(300.0, 100.0, 200.0), CardLayout::TwoColumn);
    assert_eq!(picked(300.0, 200.0, 100.0), CardLayout::Row);
}

#[test]
fn equal_coverage_goes_to_the_shorter_card() {
    assert_eq!(picked(100.0, 100.0, 300.0), CardLayout::TwoColumn);
    assert_eq!(picked(100.0, 300.0, 100.0), CardLayout::Row);
    assert_eq!(picked(100.0, 100.0, 100.0), CardLayout::Row);
}

#[test]
fn no_scan_data_drops_down() {
    assert_eq!(choose_card(&hint(), 1.0, &Fake, |_, _| None).layout, CardLayout::Dropdown);
    let (layout, _, _, covered) = card_for("somenewgame");
    assert_eq!((layout, covered), (CardLayout::Dropdown, None), "a game the scan never saw");
}

#[test]
fn each_option_is_a_key_cap_and_its_label_in_columns_of_the_layout() {
    let drop = card_scene(&hint(), CardLayout::Dropdown, 1.0, &Fake);
    assert_eq!(drop.keys.len(), 5);
    assert!(drop.keys.iter().all(|k| k.x == PAD && k.size == KEY), "one column");
    // 12 + 22 + 10 + "Performance stats" (17 × 0.6 × 13) + 12.
    assert_eq!((drop.w, drop.h), ((12.0 + 22.0 + 10.0 + 17.0 * 0.6 * 13.0 + 12.0f32).ceil(), 180.0));
    let two = card_scene(&hint(), CardLayout::TwoColumn, 1.0, &Fake);
    let left = two.keys.iter().filter(|k| k.x == PAD).count();
    assert_eq!((left, two.keys.len() - left), (3, 2), "[1][2][3] left, [4][5] right");
    assert!(two.w > drop.w && two.h < drop.h, "wider and shorter");
    let labels: Vec<&str> = two.texts.iter().filter(|t| t.role == Role::Option).map(|t| t.text.as_str()).collect();
    assert_eq!(labels, ["Bookmark", "Save 30 s", "Screenshot", "Record", "Performance stats"], "the event's order");
    let row = card_scene(&hint(), CardLayout::Row, 1.0, &Fake);
    assert!(row.w > two.w && row.h == 52.0);
}

#[test]
fn the_card_unfolds_in_120_ms_and_folds_back_from_where_it_is() {
    let now = Instant::now();
    let mut unfold = Unfold::closed(now);
    assert_eq!(unfold.progress(now), None);
    unfold.set(true, now);
    assert_eq!(unfold.progress(now), Some(0.0));
    assert!(unfold.progress(now + ms(40)).unwrap() > 0.5, "readable almost at once");
    assert_eq!(unfold.progress(now + UNFOLD), Some(1.0));
    assert!(!unfold.moving(now + UNFOLD));
    let close = now + ms(1_500);
    unfold.set(false, close);
    assert_eq!(unfold.progress(close), Some(1.0));
    assert!(unfold.progress(close + ms(60)).unwrap() < 1.0);
    assert_eq!(unfold.progress(close + UNFOLD), None, "folded away");
}

/// The real cards' sizes at 1440p (an instrument): `cargo test -p recall-overlay -- --ignored card_sizes --nocapture`.
#[test]
#[ignore]
fn card_sizes_at_1440p() {
    let fonts = crate::fonts::Fonts::new().unwrap();
    for layout in [CardLayout::Dropdown, CardLayout::TwoColumn, CardLayout::Row] {
        let card = card_scene(&hint(), layout, 1.0, &fonts);
        println!("card_size {layout:?}: {} x {}", card.w, card.h);
    }
    let pill = crate::layout::leader_scene("Alt `", 1.0, &fonts);
    println!("card_size leader pill: {} x {}", pill.w, pill.h);
}

/// The card a game gets at 1440p over a 44 px pill (stats or the leader
/// pill), from the embedded scan, with the real fonts, and what it covers.
fn card_for(key: &str) -> (CardLayout, f32, f32, Option<f32>) {
    use crate::placement::{card_cover, scale, Rect, Spots, ISLAND_H};
    let (fonts, spots) = (crate::fonts::Fonts::new().unwrap(), Spots::embedded());
    let (spot, hud) = (spots.lookup(key), spots.hud(key));
    let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    let s = scale(1440, &spot);
    let cover = |w: f32, h: f32| card_cover(&spot, hud.as_deref(), &qhd, s, (w, h), (ISLAND_H + GAP) * s);
    let card = choose_card(&hint(), s, &fonts, cover);
    (card.layout, card.w, card.h, cover(card.w, card.h))
}

/// Per game: spot, the card and the HUD area it covers (an instrument):
/// `cargo test -p recall-overlay -- --ignored per_game_cards --nocapture`.
#[test]
#[ignore]
fn per_game_cards() {
    let spots = crate::placement::Spots::embedded();
    for key in spots.keys() {
        let spot = spots.lookup(key);
        let (layout, w, h, covered) = card_for(key);
        println!("per_game {key}: {:?} {:?} -> {layout:?} {w:.0}x{h:.0}, covers {:.0} px2", spot.anchor, spot.free_px, covered.unwrap_or(-1.0));
    }
}

#[test]
fn every_scanned_game_gets_a_clean_card_or_the_least_covered_one() {
    use crate::placement::{card_cover, scale, Rect, Spots, ISLAND_H};
    let (fonts, spots) = (crate::fonts::Fonts::new().unwrap(), Spots::embedded());
    let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    for key in spots.keys() {
        let (spot, hud) = (spots.lookup(key), spots.hud(key));
        let s = scale(1440, &spot);
        let cover = |card: &CardScene| card_cover(&spot, hud.as_deref(), &qhd, s, (card.w, card.h), (ISLAND_H + GAP) * s).unwrap();
        let all = [CardLayout::Dropdown, CardLayout::TwoColumn, CardLayout::Row].map(|layout| card_scene(&hint(), layout, s, &fonts));
        let chosen = card_for(key);
        let area = chosen.3.unwrap();
        if let Some(clean) = all.iter().find(|card| cover(card) <= 0.0) {
            assert_eq!((chosen.0, area), (clean.layout, 0.0), "{key}: the first clean card");
        } else {
            assert!(all.iter().all(|card| area <= cover(card)), "{key}: no card covers less than {area}");
        }
    }
}

#[test]
fn valorant_gets_the_row_above_its_kill_feed() {
    // Five rows and three reach down into the kill feed and cover more than the
    // row, which clears it and only touches the top bar's end.
    let (layout, w, h, covered) = card_for("valorant");
    assert_eq!((layout, w, h), (CardLayout::Row, 636.0, 52.0));
    assert!(covered.unwrap() > 0.0 && covered.unwrap() < 16_000.0, "{covered:?}");
}

#[test]
fn lethal_company_drops_down_and_forza_takes_the_row() {
    // Their HUD sits in thin bands: counting only the rows that hold it decides both.
    assert_eq!(card_for("lethalcompany").0, CardLayout::Dropdown);
    assert_eq!(card_for("forzahorizon6").0, CardLayout::Row);
}
