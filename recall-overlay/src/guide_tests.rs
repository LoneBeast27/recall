use super::*;
use crate::layout::TextMetrics;
use crate::spot_edit::SNAP;

const QHD: Rect = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };

/// Every char 0.6 em wide, box 1.25 em.
struct Fake;

impl Measure for Fake {
    fn measure(&self, role: Role, scale: f32, text: &str) -> TextMetrics {
        let em = role.size() * scale;
        TextMetrics { width: text.chars().count() as f32 * em * 0.6, height: em * 1.25, baseline: em * 0.95, cap: em * 0.7 }
    }
}

#[test]
fn the_centre_lines_snap_horizontal_vertical_and_both_within_the_snap_distance_only() {
    let at = |p| centre_snap(p, &QHD, SNAP);
    assert_eq!(at((1300.0, 300.0)), ((1280.0, 300.0), Lines { vertical: true, horizontal: false }), "x onto the vertical line");
    assert_eq!(at((400.0, 700.0)), ((400.0, 720.0), Lines { vertical: false, horizontal: true }), "y onto the horizontal line");
    assert_eq!(at((1232.0, 768.0)), ((1280.0, 720.0), Lines { vertical: true, horizontal: true }), "exactly 48 px off both");
    assert_eq!(at((1328.5, 768.5)), ((1328.5, 768.5), Lines::default()), "just past the snap: free");
    // Another monitor's area: its own centre, not the primary's.
    let right = Rect { left: 2560, top: 0, right: 4480, bottom: 1080 };
    assert_eq!(centre_snap((3530.0, 530.0), &right, 36.0).0, (3520.0, 540.0));
}

#[test]
fn a_placed_spot_lights_only_the_lines_its_edge_lets_it_move_along() {
    let spot = |x, y, edge| Some(SavedSpot { x, y, edge });
    let lit = |s| lines_of(s, &QHD, &QHD);
    assert_eq!(lit(spot(0.5, 0.5, Edge::Free)), Lines { vertical: true, horizontal: true });
    assert_eq!(lit(spot(0.5, 0.0, Edge::Top)), Lines { vertical: true, horizontal: false });
    assert_eq!(lit(spot(1.0, 0.5, Edge::Right)), Lines { vertical: false, horizontal: true });
    assert_eq!(lit(spot(0.5, 0.5, Edge::Top)), Lines { vertical: true, horizontal: false }, "a top pill is never on the middle row");
    assert_eq!(lit(spot(0.51, 0.5, Edge::Bottom)), Lines::default());
    assert_eq!(lit(None), Lines::default(), "automatic: nothing lit");
}

#[test]
fn the_readout_names_the_pill_centre_in_area_percent() {
    assert_eq!(readout((1280.0, 172.8), &QHD), "x 50% · y 12%");
    assert_eq!(readout((-40.0, 9000.0), &QHD), "x 0% · y 100%", "clamped");
}

#[test]
fn the_grid_rect_is_the_placement_area() {
    let work = Rect { left: 2560, top: 0, right: 4480, bottom: 1040 };
    assert_eq!(frame(&work), ((2560, 0), (1920, 1040)), "the window sits on the area, its size");
    let g = scene(2560, 1440, 1.0, Lines::default(), None, &Fake);
    assert_eq!((g.w, g.h), (2560, 1440));
    let faint: Vec<_> = g.lines.iter().filter(|l| l.color == GRID).collect();
    assert_eq!(faint.len(), 2 * (DIVS as usize - 2), "the eighths each way, less the centre");
    assert!(faint.iter().any(|l| l.from == (320.5, 0.0) && l.to == (320.5, 1440.0)), "1/8 of the width, on a pixel centre");
    assert!(faint.iter().all(|l| l.width == 1.0 && !l.dashed));
    let centres: Vec<_> = g.lines.iter().filter(|l| l.color == CENTRE).collect();
    assert_eq!(centres.len(), 2);
    assert!(centres.iter().all(|l| l.dashed && l.width == 2.0));
    assert_eq!(g.margin, [12.5, 12.5, 2547.5, 1427.5], "the 12 px edge gap's outline");
    assert_eq!(g.labels.iter().filter(|l| l.text == "50%").count(), 2);
}

#[test]
fn a_lit_centre_line_turns_solid_accent() {
    let g = scene(2560, 1440, 1.0, Lines { vertical: true, horizontal: false }, None, &Fake);
    let down = g.lines.iter().find(|l| l.from == (1280.0, 0.0)).unwrap();
    let across = g.lines.iter().find(|l| l.from == (0.0, 720.0)).unwrap();
    assert_eq!((down.color, down.dashed), (ACCENT, false));
    assert_eq!((across.color, across.dashed), (CENTRE, true));
    assert_eq!(g.labels[0].color, ACCENT, "its 50% label too");
}

#[test]
fn the_readout_sits_under_the_pill_or_over_it_near_the_bottom() {
    let pill = |b: PillBox| scene(2560, 1440, 1.0, Lines::default(), Some((b, "x 50% · y 2%".into())), &Fake).readout.unwrap();
    let under = pill([1130, 12, 1430, 56]);
    let [l, t, r, _] = under.plate.unwrap();
    assert!(t >= 56.0 + 8.0 - 1.0 && ((l + r) / 2.0 - 1280.0).abs() <= 1.0, "under, centred: {:?}", under.plate);
    let low = pill([1130, 1384, 1430, 1428]);
    assert!(low.plate.unwrap()[3] <= 1384.0 - 8.0 + 1.0, "near the bottom: over the pill");
    let corner = pill([2400, 12, 2548, 56]);
    assert!(corner.plate.unwrap()[2] <= 2548.0, "kept inside the edge gap");
}

#[test]
fn redraws_are_whole_only_on_a_new_size_or_highlight_else_just_the_readout() {
    let at = |lines, b: PillBox, text: &str| scene(2560, 1440, 1.0, lines, Some((b, text.to_string())), &Fake);
    let first = at(Lines::default(), [1000, 300, 1300, 344], "x 45% · y 22%");
    assert_eq!(plan(None, &first), Redraw::Full, "the first draw");
    assert_eq!(plan(Some(&first), &first.clone()), Redraw::None, "nothing changed: nothing drawn");
    let moved = at(Lines::default(), [1010, 300, 1310, 344], "x 45% · y 22%");
    let Redraw::Part([l, t, r, b]) = plan(Some(&first), &moved) else { panic!("a moved readout redraws its boxes only") };
    let (old, new) = (first.readout.as_ref().unwrap().plate.unwrap(), moved.readout.as_ref().unwrap().plate.unwrap());
    assert!(l as f32 <= old[0] && r as f32 >= new[2] && t as f32 <= old[1] && b as f32 >= old[3], "covers both plates");
    assert!((r - l) < 300 && (b - t) < 60, "a small box, not the screen: {:?}", [l, t, r, b]);
    let lit = at(Lines { vertical: true, horizontal: false }, [1010, 300, 1310, 344], "x 50% · y 22%");
    assert_eq!(plan(Some(&moved), &lit), Redraw::Full, "a line lit");
    let resized = scene(1920, 1080, 0.75, Lines::default(), None, &Fake);
    assert_eq!(plan(Some(&first), &resized), Redraw::Full, "a new size");
}

/// `RECALL_GRID_PNG=path`: the grid and the editor's pill (x 50% · y 12%,
/// the vertical line lit) at 1x over a 2560x1440 area, through the real
/// renderers and fonts. No window.
#[test]
#[ignore]
fn grid_png() {
    use crate::anim::Frame;
    use crate::fonts::Fonts;
    use crate::guide_render::GuideRenderer;
    use crate::notice::{NoticeView, Tone};
    use crate::presence::Look;
    use crate::render::{Layers, Renderer};
    use crate::spot_edit::{origin, LookBox};
    let Some(path) = std::env::var_os("RECALL_GRID_PNG").map(std::path::PathBuf::from) else { return };
    let (fonts, mut island, mut grid) = (Fonts::new().unwrap(), Renderer::new().unwrap(), GuideRenderer::new().unwrap());
    let notice = NoticeView::new(Tone::Neutral, crate::app::editor::HINT_TITLE, crate::app::editor::HINT_DETAIL, false);
    let (pill_scene, k, (w, h)) = crate::island::notice_layout(&notice, 1.0, 1.0, || None, &fonts);
    let look = Look { align: 0.5, ..Look::open(Frame { w, h, alpha: 1.0 }) };
    let spot = SavedSpot { x: 0.5, y: 0.12, edge: Edge::Free };
    let b = LookBox { along: look.span_w(), depth: look.extent(), pill: look.box_h, margin: look.margin };
    let (x, y) = origin(&spot, &QHD, &QHD, b, 12);
    let (pw, ph) = island.draw(&fonts, &pill_scene, look, k, Layers::default()).unwrap();
    let bounds = [x, y, x + pw, y + ph];
    let centre = ((x as f32 + pw as f32 / 2.0), (y as f32 + ph as f32 / 2.0));
    let g = scene(2560, 1440, 1.0, lines_of(Some(spot), &QHD, &QHD), Some((bounds, readout(centre, &QHD))), &fonts);
    grid.draw(&fonts, &g, None).unwrap();
    // A dim game-like backdrop, then the grid, then the pill (premultiplied over).
    let (cw, ch) = (2560usize, 1440usize);
    let mut rgba: Vec<u8> = (0..cw * ch).flat_map(|i| {
        let (px, py) = ((i % cw) as f32 / cw as f32, (i / cw) as f32 / ch as f32);
        [(34.0 + 30.0 * px) as u8, (40.0 + 18.0 * py) as u8, (52.0 + 20.0 * (1.0 - px)) as u8, 255]
    }).collect();
    let mut over = |src: &[[u8; 4]], (ox, oy): (usize, usize), sw: usize| {
        for (i, [b, g, r, a]) in src.iter().copied().enumerate() {
            let (px, py) = (ox + i % sw, oy + i / sw);
            if px >= cw || py >= ch {
                continue;
            }
            let o = (py * cw + px) * 4;
            let keep = 1.0 - a as f32 / 255.0;
            for (c, v) in [r, g, b].into_iter().enumerate() {
                rgba[o + c] = (v as f32 + rgba[o + c] as f32 * keep).round().min(255.0) as u8;
            }
        }
    };
    over(&grid.pixels(), (0, 0), cw);
    over(&island.pixels(pw, ph), (x as usize, y as usize), pw as usize);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(&path, crate::png::encode(cw as u32, ch as u32, &rgba)).unwrap();
    println!("wrote {} (pill {pw}x{ph} at {x},{y}; readout {:?})", path.display(), g.readout.map(|r| r.text));
}
