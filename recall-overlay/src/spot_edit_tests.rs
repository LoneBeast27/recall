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
fn dragging_and_releasing_saves_and_right_click_puts_the_old_spot_back() {
    let t0 = Instant::now();
    let old = SavedSpot { x: 0.5, y: 0.0, edge: Edge::Top };
    let mut editor = Editor::new("rocketleague", Some(old), t0);
    assert_eq!(effective(Some(&editor), Some(old)), Some(old), "editing starts where the island is");
    assert!(!editor.drag((100.0, 100.0), (300.0, 44.0), &QHD, &QHD, SNAP, t0), "no drag before the button goes down");
    assert_eq!(editor.release(t0), None, "a release with no drag writes nothing");
    editor.press((1290.0, 40.0), (1280.0, 34.0), t0);
    assert!(editor.drag((2390.0, 726.0), (300.0, 44.0), &QHD, &QHD, SNAP, t0));
    let moved = editor.spot.unwrap();
    assert_eq!((moved.edge, moved.x, moved.y), (Edge::Right, 2380.0 / 2560.0, 0.5), "the grab point is kept");
    assert_eq!(effective(Some(&editor), Some(old)), Some(moved), "the island follows the drag live");
    assert_eq!(editor.release(t0), Some(moved), "releasing saves the spot");
    assert_eq!(editor.finish(EditKey::Leave), None, "Alt+U after a release writes nothing more (it is saved)");
    assert_eq!(editor.finish(EditKey::Cancel), Some(Some(old)), "right-click writes the old spot back");
    assert_eq!(editor.finish(EditKey::Reset), Some(None), "the ⟲ button clears it");
    let untouched = Editor::new("rocketleague", Some(old), t0);
    assert_eq!(untouched.finish(EditKey::Cancel), None, "nothing saved yet: nothing to put back");
}

/// P0-1: a press whose release never arrives (the button came up over
/// another window, or the capture went elsewhere) must not trap the editor.
#[test]
fn a_press_with_no_release_ends_after_the_idle_minute_without_saving() {
    let t0 = Instant::now();
    let old = SavedSpot { x: 0.5, y: 0.0, edge: Edge::Top };
    let mut editor = Editor::new("valorant", Some(old), t0);
    editor.press((1290.0, 40.0), (1280.0, 34.0), t0);
    editor.drag((1290.0, 700.0), (300.0, 44.0), &QHD, &QHD, SNAP, t0);
    assert!(editor.dragging());
    assert_eq!(editor.watch(t0 + IDLE - Duration::from_millis(1), true, true), None);
    assert_eq!(editor.watch(t0 + IDLE, true, true), Some(EditKey::Leave), "held but idle: leave");
    let mut window = Fake::default();
    assert_eq!(end(&mut window, &mut editor, EditKey::Leave), None, "the drag is not saved");
    assert_eq!(editor.spot, Some(old), "the pill goes back to the saved spot");
    assert!(!editor.dragging() && window.click_through == Some(true) && window.released);
}

#[test]
fn a_move_without_the_button_drops_the_drag_and_keeps_editing() {
    let t0 = Instant::now();
    let mut editor = Editor::new("valorant", None, t0);
    assert!(!editor.drop_drag(), "nothing to drop");
    editor.press((1290.0, 40.0), (1280.0, 34.0), t0);
    editor.drag((1290.0, 700.0), (300.0, 44.0), &QHD, &QHD, SNAP, t0);
    assert!(editor.drop_drag());
    assert_eq!((editor.spot, editor.dragging()), (None, false), "back to automatic, nothing written");
    assert_eq!(editor.release(t0), None, "a late release saves nothing");
}

#[test]
fn focus_loss_capture_loss_and_the_two_minute_cap_end_the_editor_even_mid_drag() {
    let t0 = Instant::now();
    let mut editor = Editor::new("valorant", None, t0);
    assert_eq!(editor.watch(t0, true, false), None, "no capture is needed while not dragging");
    editor.press((0.0, 0.0), (0.0, 0.0), t0);
    assert_eq!(editor.watch(t0, false, true), Some(EditKey::Leave), "the game lost focus mid-drag");
    assert_eq!(editor.watch(t0, true, false), Some(EditKey::Leave), "the capture went elsewhere mid-drag");
    // Moving all along keeps it from idling, but not past the cap.
    let mut now = t0;
    while now < t0 + CAP - Duration::from_secs(5) {
        now += Duration::from_secs(5);
        editor.drag((now.duration_since(t0).as_secs_f32(), 700.0), (300.0, 44.0), &QHD, &QHD, SNAP, now);
        assert_eq!(editor.watch(now, true, true), None, "busy dragging at {:?}", now - t0);
    }
    assert_eq!(editor.watch(t0 + CAP, true, true), Some(EditKey::Leave), "120 s from the start, dragging or not");
}

#[derive(Default)]
struct Fake {
    click_through: Option<bool>,
    released: bool,
    /// The guide grid's window exists.
    guide: bool,
    guides_made: usize,
    calls: usize,
}

impl EditWindow for Fake {
    fn set_click_through(&mut self, on: bool) {
        self.click_through = Some(on);
        self.calls += 1;
    }
    fn release_capture(&mut self) {
        self.released = true;
        self.calls += 1;
    }
    fn show_guide(&mut self, on: bool) {
        self.guides_made += (on && !self.guide) as usize;
        self.guide = on;
        self.calls += 1;
    }
}

/// P0-2: exclusive fullscreen (and every other refusal) never touches the window.
#[test]
fn a_refusal_leaves_the_window_style_untouched() {
    let t0 = Instant::now();
    assert_eq!(refusal(true, true, true, true), Some("the game runs in exclusive fullscreen"));
    assert_eq!(refusal(true, true, true, false), None);
    for refused in [refusal(false, true, true, false), refusal(true, false, true, false), refusal(true, true, false, false), refusal(true, true, true, true)] {
        let mut window = Fake::default();
        assert!(begin(&mut window, refused, "rocketleague", None, t0).is_err());
        assert_eq!(window.calls, 0, "{refused:?}: the style is never changed");
    }
}

/// Every way out puts the window back to click-through and lets the mouse go.
#[test]
fn every_exit_path_restores_click_through() {
    let t0 = Instant::now();
    let spot = SavedSpot { x: 0.2, y: 0.0, edge: Edge::Top };
    type Path = (&'static str, fn(&mut Editor, Instant) -> Option<EditKey>);
    let paths: [Path; 8] = [
        ("save then Alt+U", |e, t| {
            e.press((0.0, 0.0), (0.0, 0.0), t);
            e.release(t);
            Some(EditKey::Leave)
        }),
        ("right-click cancel", |_, _| Some(EditKey::Cancel)),
        ("reset button", |_, _| Some(EditKey::Reset)),
        ("Alt+U", |_, _| Some(EditKey::Leave)),
        ("idle", |e, t| e.watch(t + IDLE, true, true)),
        ("focus loss", |e, t| e.watch(t, false, true)),
        ("120 s cap", |e, t| {
            e.press((0.0, 0.0), (0.0, 0.0), t + CAP);
            e.watch(t + CAP, true, true)
        }),
        ("capture lost", |e, t| {
            e.press((0.0, 0.0), (0.0, 0.0), t);
            e.watch(t, true, false)
        }),
    ];
    for (name, path) in paths {
        let mut window = Fake::default();
        let mut editor = begin(&mut window, None, "rocketleague", Some(spot), t0).unwrap();
        assert_eq!(window.click_through, Some(false), "{name}: editing takes the mouse");
        assert!(window.guide, "{name}: the grid shows while editing");
        let how = path(&mut editor, t0).unwrap_or_else(|| panic!("{name}: the editor must end"));
        end(&mut window, &mut editor, how);
        assert_eq!((window.click_through, window.released), (Some(true), true), "{name}: click-through again");
        assert!(!editor.dragging(), "{name}: no drag left");
        assert!(!window.guide, "{name}: the grid window is gone");
        assert_eq!(window.guides_made, 1, "{name}: one grid per editing run");
    }
}

/// The centre lines pull the pill's centre in from the same distance edges
/// snap at; past it the pill stays where it is dropped.
#[test]
fn a_dragged_pill_snaps_its_centre_onto_the_grid_centre_lines() {
    let t0 = Instant::now();
    let drag_to = |cursor: (f32, f32)| {
        let mut editor = Editor::new("valorant", None, t0);
        editor.press((0.0, 0.0), (0.0, 0.0), t0);
        editor.drag(cursor, (300.0, 44.0), &QHD, &QHD, SNAP, t0);
        editor.spot.unwrap()
    };
    let vertical = drag_to((1280.0 + 40.0, 400.0));
    assert_eq!((vertical.x, vertical.y, vertical.edge), (0.5, 400.0 / 1440.0, Edge::Free), "x onto the line down the middle");
    let horizontal = drag_to((600.0, 720.0 - 47.0));
    assert_eq!((horizontal.x, horizontal.y), (600.0 / 2560.0, 0.5), "y onto the line across");
    let both = drag_to((1280.0 - 30.0, 720.0 + 30.0));
    assert_eq!((both.x, both.y), (0.5, 0.5), "both: the dead centre");
    let free = drag_to((1280.0 + 49.0, 720.0 - 49.0));
    assert_eq!((free.x, free.y), (1329.0 / 2560.0, 671.0 / 1440.0), "49 px off both: no snap");
    let top = drag_to((1280.0 + 20.0, 30.0));
    assert_eq!((top.x, top.edge), (0.5, Edge::Top), "an edge snap and a centre snap together");
    // The grid is the placement area: a work area above a 40 px taskbar centres higher.
    let work = Rect { left: 0, top: 0, right: 2560, bottom: 1400 };
    let mut editor = Editor::new("valorant", None, t0);
    editor.press((0.0, 0.0), (0.0, 0.0), t0);
    editor.drag((900.0, 705.0), (300.0, 44.0), &QHD, &work, SNAP, t0);
    assert_eq!(editor.spot.unwrap().y, 700.0 / 1440.0, "onto the work area's centre (700), in monitor fractions");
}
#[test]
fn cancel_puts_the_old_size_back_and_the_other_exits_keep_the_new_one() {
    let t0 = Instant::now();
    let mut editor = Editor::new("valorant", None, t0).with_size(1.1);
    assert_eq!(editor.finish_size(EditKey::Cancel), None, "nothing changed: nothing to write");
    editor.size = 1.3;
    assert_eq!(editor.finish_size(EditKey::Cancel), Some(1.1));
    assert_eq!(editor.finish_size(EditKey::Leave), None, "Done keeps what was written");
    assert_eq!(editor.finish_size(EditKey::Reset), None, "Auto clears the spot only");
}

#[test]
fn a_one_click_place_is_saved_like_a_release_and_cancel_puts_the_old_spot_back() {
    let t0 = Instant::now();
    let old = SavedSpot { x: 0.2, y: 0.0, edge: Edge::Top };
    let mut editor = Editor::new("valorant", Some(old), t0);
    let right = SavedSpot { x: 1.0, y: 0.5, edge: Edge::Right };
    assert_eq!(editor.place(right, t0), right);
    assert_eq!(editor.spot, Some(right));
    assert_eq!(editor.finish(EditKey::Leave), None, "already written on the click");
    assert_eq!(editor.finish(EditKey::Cancel), Some(Some(old)));
    assert!(!editor.drop_drag(), "a place is not a drag");
}
