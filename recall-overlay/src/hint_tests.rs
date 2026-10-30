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

#[test]
fn the_card_drops_down_where_it_fits_else_two_columns_else_one_row() {
    let pick = |room: &dyn Fn(f32) -> Option<f32>| choose_card(&hint(), 1.0, &Fake, room).layout;
    // The same room whatever the width (a scan with one free depth).
    assert_eq!(pick(&|_| Some(226.0)), CardLayout::Dropdown, "fits");
    assert_eq!(pick(&|_| Some(180.0)), CardLayout::Dropdown, "exactly fits");
    assert_eq!(pick(&|_| Some(138.0)), CardLayout::TwoColumn, "too shallow for five rows");
    assert_eq!(pick(&|_| Some(60.0)), CardLayout::Row, "too shallow for three rows");
    assert_eq!(pick(&|_| Some(10.0)), CardLayout::Row, "nothing fits: the shortest");
    assert_eq!(pick(&|_| None), CardLayout::Dropdown, "no scan data: drop down");
    // A kill feed under narrow cards, a top bar beside wide ones: each card is
    // judged on its own columns.
    let drop_w = card_scene(&hint(), CardLayout::Dropdown, 1.0, &Fake).w;
    assert_eq!(pick(&|w| Some(if w <= drop_w { 62.0 } else { 200.0 })), CardLayout::TwoColumn);
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

/// The card each scanned game gets at 1440p over a 44 px pill (stats or the
/// leader pill), from the embedded scan, with the real fonts.
fn layout_for(key: &str) -> (CardLayout, f32, f32, Option<f32>) {
    use crate::placement::{card_room, scale, Rect, Spots, ISLAND_H};
    let (fonts, spots) = (crate::fonts::Fonts::new().unwrap(), Spots::embedded());
    let (spot, sky) = (spots.lookup(key), spots.skyline(key));
    let qhd = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    let s = scale(1440, &spot);
    let room = |w: f32| card_room(&spot, sky.as_deref(), &qhd, s, w, ISLAND_H * s, GAP * s);
    let card = choose_card(&hint(), s, &fonts, room);
    (card.layout, card.w, card.h, room(card.w))
}

/// Per game: spot, free depth and the card it gets (an instrument):
/// `cargo test -p recall-overlay -- --ignored per_game_cards --nocapture`.
#[test]
#[ignore]
fn per_game_cards() {
    let spots = crate::placement::Spots::embedded();
    for key in spots.keys() {
        let spot = spots.lookup(key);
        let (layout, w, h, room) = layout_for(key);
        let verdict = if room.is_none_or(|r| h <= r) { "fits" } else { "fallback (overlaps)" };
        println!("per_game {key}: {:?} free {:?} -> {layout:?} {w}x{h}, room {room:?}: {verdict}", spot.anchor, spot.free_px);
    }
}

#[test]
fn valorant_gets_the_single_row_above_its_kill_feed() {
    // 124 free rows under the island (the kill feed), 62 of them under the pill:
    // five rows (180) and three (116) do not fit; the row (52) does in height.
    let (layout, _, h, _) = layout_for("valorant");
    assert_eq!((layout, h), (CardLayout::Row, 52.0));
}
