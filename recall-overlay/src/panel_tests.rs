use super::*;
use crate::layout::TextMetrics;

const QHD: Rect = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };

/// Every char 0.6 em wide, box 1.25 em.
struct Fake;

impl Measure for Fake {
    fn measure(&self, role: Role, scale: f32, text: &str) -> TextMetrics {
        let em = role.size() * scale;
        TextMetrics { width: text.chars().count() as f32 * em * 0.6, height: em * 1.25, baseline: em * 0.95, cap: em * 0.7 }
    }
}

fn state(size: f32, edge: Option<Edge>) -> PanelState<'static> {
    PanelState { game: "valorant", size, edge }
}

fn button(scene: &PanelScene, action: Action) -> &Button {
    scene.buttons.iter().find(|b| b.action == action).unwrap_or_else(|| panic!("{action:?} is on the panel"))
}

fn centre(b: &Button) -> (f32, f32) {
    ((b.rect[0] + b.rect[2]) / 2.0, (b.rect[1] + b.rect[3]) / 2.0)
}

#[test]
fn the_size_steps_by_five_percent_between_75_and_150() {
    assert_eq!(step_size(1.0, true), 1.05);
    assert_eq!(step_size(1.0, false), 0.95);
    assert_eq!(step_size(0.75, false), 0.75, "the smallest stays");
    assert_eq!(step_size(1.5, true), 1.5, "the largest stays");
    assert_eq!(step_size(0.97, true), 1.0, "off the grid: up lands on it");
    assert_eq!(step_size(0.97, false), 0.95, "off the grid: down lands on it");
    let mut size = 1.0;
    for _ in 0..30 {
        size = step_size(size, true);
    }
    assert_eq!(size, 1.5, "many steps never pass the end");
}

#[test]
fn an_unusable_saved_size_counts_as_100_percent() {
    for bad in [f32::NAN, f32::INFINITY, 0.0, 0.5, 3.0, -1.0] {
        assert!(!sane_size(bad), "{bad}");
        assert_eq!(size_pct(bad), 100, "{bad}");
    }
    assert!(sane_size(0.75) && sane_size(1.5) && sane_size(1.2));
}

#[test]
fn every_button_is_hit_at_its_centre_and_nowhere_off_the_buttons() {
    let scene = scene(&state(1.2, Some(Edge::Top)), 1.0, &Fake);
    for b in &scene.buttons {
        assert_eq!(scene.hit(centre(b)), Some(b.action), "{:?}", b.action);
    }
    assert_eq!(scene.hit((1.0, 1.0)), None, "the corner is not a button");
    let title = &scene.texts[0];
    assert_eq!(scene.hit((title.x + 2.0, title.y + 2.0)), None, "the title is not a button");
}

#[test]
fn the_size_ends_and_100_percent_disable_their_buttons() {
    let small = scene(&state(0.75, None), 1.0, &Fake);
    assert_eq!(button(&small, Action::Smaller).tone, Tone::Disabled);
    assert_eq!(small.hit(centre(button(&small, Action::Smaller))), None, "a disabled button is never hit");
    assert_eq!(button(&small, Action::Larger).tone, Tone::Plain);
    let large = scene(&state(1.5, None), 1.0, &Fake);
    assert_eq!(button(&large, Action::Larger).tone, Tone::Disabled);
    let auto = scene(&state(1.0, None), 1.0, &Fake);
    assert_eq!(button(&auto, Action::SizeReset).tone, Tone::Disabled, "100% has nothing to reset");
    assert!(auto.texts.iter().any(|t| t.text == "100%"), "the readout shows the size");
}

#[test]
fn the_place_the_pill_is_on_is_lit_and_auto_is_lit_without_one() {
    let top = scene(&state(1.0, Some(Edge::Left)), 1.0, &Fake);
    let lit: Vec<Action> = top.buttons.iter().filter(|b| b.tone == Tone::Active).map(|b| b.action).collect();
    assert_eq!(lit, vec![Action::Place(Edge::Left)]);
    let auto = scene(&state(1.0, None), 1.0, &Fake);
    let lit: Vec<Action> = auto.buttons.iter().filter(|b| b.tone == Tone::Active).map(|b| b.action).collect();
    assert_eq!(lit, vec![Action::Auto]);
    assert_eq!(button(&auto, Action::Done).tone, Tone::Accent);
}

#[test]
fn buttons_never_overlap_and_all_sit_inside_the_panel() {
    for k in [0.75, 1.0, 1.5, 2.0] {
        let scene = scene(&state(1.0, None), k, &Fake);
        let (w, h) = (scene.w as f32, scene.h as f32);
        for (i, a) in scene.buttons.iter().enumerate() {
            assert!(a.rect[0] >= 0.0 && a.rect[1] >= 0.0 && a.rect[2] <= w && a.rect[3] <= h, "k {k}: {:?} inside", a.action);
            for b in &scene.buttons[i + 1..] {
                let apart = a.rect[2] <= b.rect[0] || b.rect[2] <= a.rect[0] || a.rect[3] <= b.rect[1] || b.rect[3] <= a.rect[1];
                assert!(apart, "k {k}: {:?} and {:?} overlap", a.action, b.action);
            }
        }
        // Done ends on the right padding, like the widest row.
        assert!((button(&scene, Action::Done).rect[2] - (w - (PAD * k).round())).abs() <= 1.0, "k {k}: Done is right-aligned");
    }
}

#[test]
fn the_panel_grows_with_the_monitor_scale_only() {
    let qhd = scene(&state(1.0, None), 1.0, &Fake);
    let uhd = scene(&state(1.0, None), 1.5, &Fake);
    let ratio = uhd.w as f32 / qhd.w as f32;
    assert!((ratio - 1.5).abs() < 0.03, "4K is 1.5x the 1440p panel: {ratio}");
    let bigger_island = scene(&state(1.5, None), 1.0, &Fake);
    assert_eq!((bigger_island.w, bigger_island.h), (qhd.w, qhd.h), "the island size never changes the panel's");
}

#[test]
fn one_click_places_centre_the_pill_along_the_edge_of_the_area() {
    let top = placed_spot(Edge::Top, &QHD, &QHD);
    assert_eq!((top.x, top.edge), (0.5, Edge::Top));
    let right = placed_spot(Edge::Right, &QHD, &QHD);
    assert_eq!((right.x, right.y, right.edge), (1.0, 0.5, Edge::Right));
    let centre = placed_spot(Edge::Free, &QHD, &QHD);
    assert_eq!((centre.x, centre.y, centre.edge), (0.5, 0.5, Edge::Free));
    // A windowed game's area on a second monitor: its own centre, in that monitor's fractions.
    let monitor = Rect { left: 2560, top: 0, right: 4480, bottom: 1080 };
    let area = Rect { left: 2560, top: 0, right: 3520, bottom: 540 };
    let bottom = placed_spot(Edge::Bottom, &monitor, &area);
    assert_eq!((bottom.x, bottom.y, bottom.edge), (0.25, 0.5, Edge::Bottom));
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right, Edge::Free] {
        assert!(placed_spot(edge, &monitor, &area).sane(), "{edge:?}");
    }
}

#[test]
fn the_panel_opens_on_the_far_side_from_the_pill_and_inside_the_area() {
    let size = (400, 160);
    let top_pill = [1180, 20, 1380, 64];
    let (x, y) = origin(size, &QHD, top_pill, 1.0);
    assert_eq!(x, 1280 - 200, "centred across");
    assert!(y > 720, "below the middle: {y}");
    let bottom_pill = [1180, 1376, 1380, 1420];
    assert!(origin(size, &QHD, bottom_pill, 1.0).1 + size.1 < 720, "above the middle");
    let right_pill = [2496, 600, 2540, 840];
    let (x, _) = origin(size, &QHD, right_pill, 1.0);
    assert!(x + size.0 < 1280, "left of the middle: {x}");
    // A tiny area: still inside, pushed to its edge gap.
    let small = Rect { left: 100, top: 100, right: 500, bottom: 300 };
    let (x, y) = origin(size, &small, [280, 110, 320, 130], 1.0);
    assert!(x >= 100 && y >= 100, "inside the area's top-left: {x},{y}");
}
