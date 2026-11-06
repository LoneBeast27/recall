use super::*;

const QHD: Rect = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };

fn b(along: f32, depth: f32) -> LookBox {
    LookBox { along, depth, pill: depth, margin: 0.0 }
}

#[test]
fn a_pill_dropped_near_an_edge_snaps_to_it_and_elsewhere_stays_free() {
    let near = |cx, cy| snap((cx, cy), (300.0, 44.0), &QHD, SNAP).edge;
    assert_eq!(near(1280.0, 40.0), Edge::Top, "18 px from the top");
    assert_eq!(near(1280.0, 1400.0), Edge::Bottom);
    assert_eq!(near(180.0, 700.0), Edge::Left, "30 px from the left");
    assert_eq!(near(2380.0, 700.0), Edge::Right);
    assert_eq!(near(1280.0, 700.0), Edge::Free, "the middle of the screen");
    assert_eq!(near(1280.0, 100.0), Edge::Free, "78 px from the top is past the snap");
    let spot = snap((640.0, 360.0), (300.0, 44.0), &QHD, SNAP);
    assert_eq!((spot.x, spot.y), (0.25, 0.25), "monitor fractions");
    let off = snap((-500.0, 9000.0), (300.0, 44.0), &QHD, SNAP);
    assert!(off.sane(), "dragged off screen: clamped into the monitor");
}

#[test]
fn a_side_spot_stacks_the_island_and_hugs_its_edge() {
    let right = SavedSpot { x: 0.98, y: 0.5, edge: Edge::Right };
    assert_eq!(right.anchor(), Anchor::RightMiddle, "the vertical stacked layout");
    assert_eq!(SavedSpot { edge: Edge::Left, ..right }.anchor(), Anchor::LeftMiddle);
    // Tall pill: 44 deep, 300 along the edge, 12 px in from the right, centred on y.
    assert_eq!(origin(&right, &QHD, &QHD, b(300.0, 44.0), 12), (2560 - 12 - 44, 720 - 150));
    let top = SavedSpot { x: 0.25, y: 0.0, edge: Edge::Top };
    assert_eq!(origin(&top, &QHD, &QHD, b(300.0, 44.0), 12), (640 - 150, 12));
    let bottom = SavedSpot { x: 0.5, y: 1.0, edge: Edge::Bottom };
    assert_eq!(origin(&bottom, &QHD, &QHD, b(300.0, 44.0), 12), (1280 - 150, 1440 - 12 - 44), "grows upwards");
    let free = SavedSpot { x: 0.5, y: 0.5, edge: Edge::Free };
    assert_eq!(origin(&free, &QHD, &QHD, b(300.0, 44.0), 12), (1130, 698), "the pill centred on the saved point");
    // Never off the screen, and the margin wraps the box.
    let corner = SavedSpot { x: 1.0, y: 0.0, edge: Edge::Free };
    let with_margin = LookBox { margin: 3.0, ..b(300.0, 44.0) };
    assert_eq!(origin(&corner, &QHD, &QHD, with_margin, 12), (2560 - 300 - 3, -3));
}

#[test]
fn a_saved_spot_wins_over_the_hud_pick() {
    let saved = SavedSpot { x: 0.1, y: 0.5, edge: Edge::Left };
    let (spot, custom) = dynamic_spot(Some(saved), || panic!("the HUD pick is never asked"));
    assert_eq!((spot.anchor, custom), (Anchor::LeftMiddle, Some(saved)));
    let (spot, custom) = dynamic_spot(None, || Spot { anchor: Anchor::TopLeft, free_px: None });
    assert_eq!((spot.anchor, custom), (Anchor::TopLeft, None), "no saved spot: the HUD pick decides");
}

#[test]
fn dragging_moves_the_spot_and_esc_restores_the_old_one() {
    let t0 = Instant::now();
    let old = SavedSpot { x: 0.5, y: 0.0, edge: Edge::Top };
    let mut editor = Editor::new("rocketleague", Some(old), t0);
    assert_eq!(effective(Some(&editor), Some(old)), Some(old), "editing starts where the island is");
    assert!(!editor.drag((100.0, 100.0), (300.0, 44.0), &QHD, SNAP, t0), "no drag before the button goes down");
    editor.press((1290.0, 40.0), (1280.0, 34.0), t0);
    assert!(editor.drag((2390.0, 726.0), (300.0, 44.0), &QHD, SNAP, t0));
    let moved = editor.spot.unwrap();
    assert_eq!((moved.edge, moved.x, moved.y), (Edge::Right, 2380.0 / 2560.0, 0.5), "the grab point is kept");
    assert_eq!(effective(Some(&editor), Some(old)), Some(moved), "the island follows the drag live");
    editor.release(t0);
    // Esc: nothing is written and the island goes back to the saved spot.
    assert_eq!(editor.finish(EditKey::Cancel), None);
    assert_eq!(effective(None, Some(old)), Some(old));
    // Enter writes the moved spot; R clears the game's spot.
    assert_eq!(editor.finish(EditKey::Save), Some(Some(moved)));
    assert_eq!(editor.finish(EditKey::Reset), Some(None));
}

#[test]
fn the_editor_gives_up_after_a_minute_idle_but_never_mid_drag() {
    let t0 = Instant::now();
    let mut editor = Editor::new("valorant", None, t0);
    assert!(!editor.timed_out(t0 + IDLE - Duration::from_millis(1)));
    assert!(editor.timed_out(t0 + IDLE));
    editor.press((0.0, 0.0), (0.0, 0.0), t0);
    assert!(!editor.timed_out(t0 + IDLE * 2), "holding the pill is not idle");
    editor.release(t0 + IDLE * 2);
    assert!(!editor.timed_out(t0 + IDLE * 2 + Duration::from_secs(59)));
}

#[test]
fn arrow_keys_place_the_pill_without_a_mouse() {
    // Exclusive fullscreen: the game owns the cursor, so the arrows move the pill.
    let t0 = Instant::now();
    let mut editor = Editor::new("rocketleague", None, t0);
    let shown = (300.0, 44.0);
    assert!(editor.nudge((NUDGE, 0.0), (1280.0, 720.0), shown, &QHD, SNAP, t0));
    assert_eq!(editor.spot, Some(SavedSpot { x: 1320.0 / 2560.0, y: 0.5, edge: Edge::Free }));
    // Walking up to the top edge snaps to it.
    let mut centre = (1280.0, 720.0);
    while editor.spot.map(|s| s.edge) != Some(Edge::Top) {
        centre.1 -= NUDGE;
        editor.nudge((0.0, -NUDGE), (centre.0, centre.1 + NUDGE), shown, &QHD, SNAP, t0);
        assert!(centre.1 > 0.0, "reaches the top edge");
    }
    assert!(editor.timed_out(t0 + IDLE) && !editor.timed_out(t0 + IDLE - Duration::from_secs(1)), "a key press counts as input");
    assert_eq!(editor.finish(EditKey::Save), Some(editor.spot), "Enter keeps it");
}
