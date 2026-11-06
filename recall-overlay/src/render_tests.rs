use super::*;
use crate::anim::Frame;

fn alpha_at(renderer: &Renderer, w: i32, h: i32, x: i32, y: i32) -> u8 {
    renderer.pixels(w, h)[(y * w + x) as usize][3]
}

#[test]
fn pill_is_antialiased_and_survives_the_buffer_growing() {
    let (fonts, mut renderer) = (Fonts::new().unwrap(), Renderer::new().unwrap());
    let scene = Scene { w: 60.0, h: 44.0, ..Scene::default() };
    let (w, h) = renderer.draw(&fonts, &scene, Look::open(Frame { w: 60.0, h: 44.0, alpha: 1.0 }), 1.0, Layers::default()).unwrap();
    assert_eq!(alpha_at(&renderer, w, h, 30, 22), (PILL.a * 255.0).round() as u8, "pill body is 94% opaque");
    assert_eq!(alpha_at(&renderer, w, h, 0, 0), 0, "rounded corner is transparent");
    let edge = alpha_at(&renderer, w, h, 4, 8);
    assert!(edge > 0 && edge < 240, "the curve is anti-aliased: {edge}");
    // A wider frame re-creates the DIB; drawing must land in the new one.
    let (w, h) = renderer.draw(&fonts, &scene, Look::open(Frame { w: 300.0, h: 44.0, alpha: 1.0 }), 1.0, Layers::default()).unwrap();
    assert_eq!(w, 300);
    assert_eq!(alpha_at(&renderer, w, h, 280, 22), (PILL.a * 255.0).round() as u8);
}

#[test]
fn a_side_spot_runs_the_pill_down_the_edge_mark_on_top() {
    use crate::layout::{Shape, SIGNAL};
    let (fonts, mut renderer) = (Fonts::new().unwrap(), Renderer::new().unwrap());
    // A mark 20 px into the pill's left end.
    let dot = Shape::Dot { cx: 20.0, cy: 22.0, r: 6.0, color: SIGNAL };
    let scene = Scene { w: 300.0, h: 44.0, shapes: vec![dot], ..Scene::default() };
    let frame = Frame { w: 300.0, h: 44.0, alpha: 1.0 };
    let red = |r: &Renderer, w: i32, h: i32, x: i32, y: i32| {
        let [b, g, red, a] = r.pixels(w, h)[(y * w + x) as usize];
        red > 200 && g < 120 && b < 120 && a > 240
    };
    let flat = renderer.draw(&fonts, &scene, Look::open(frame), 1.0, Layers::default()).unwrap();
    assert_eq!(flat, (300, 44));
    assert!(red(&renderer, 300, 44, 20, 22), "unturned: the mark at the left end");
    let cw = Look { turn: Turn::Right, ..Look::open(frame) };
    assert_eq!(renderer.draw(&fonts, &scene, cw, 1.0, Layers::default()).unwrap(), (44, 300), "the window turns");
    assert!(red(&renderer, 44, 300, 22, 20), "right edge: the mark on top");
    assert_eq!(alpha_at(&renderer, 44, 300, 22, 150), (PILL.a * 255.0).round() as u8, "the body runs down the middle");
    assert_eq!(alpha_at(&renderer, 44, 300, 0, 0), 0, "rounded ends stay clear");
    let ccw = Look { turn: Turn::Left, ..Look::open(frame) };
    assert_eq!(renderer.draw(&fonts, &scene, ccw, 1.0, Layers::default()).unwrap(), (44, 300));
    assert!(red(&renderer, 44, 300, 22, 20), "left edge: the mark on top too");
    // The next unturned frame is flat again.
    renderer.draw(&fonts, &scene, Look::open(frame), 1.0, Layers::default()).unwrap();
    assert!(red(&renderer, 300, 44, 20, 22));
}

/// Software-render cost of one island frame (an instrument, not a gate):
/// `cargo test --release -p recall-overlay -- --ignored draw_cost --nocapture`.
#[test]
#[ignore]
fn draw_cost_per_frame() {
    use crate::badge::SaveMark;
    use crate::layout::{notice_scene, stats_scene, StatsContent, ACTIVE};
    use crate::notice::{NoticeView, SavePhase, Tone};
    let (fonts, mut renderer) = (Fonts::new().unwrap(), Renderer::new().unwrap());
    let stats = StatsContent { recording: true, replay_secs: 120, fps: Some(144), low1: Some(98), gpu: Some(71), gpu_temp: Some(64), cpu: Some(23), ..Default::default() };
    let stats = stats_scene(&stats, 1.0, &fonts);
    let grow = 58.0 / 44.0;
    let notice = notice_scene(&NoticeView::new(Tone::Success, "Clip saved", "VALORANT · 0:30", true), grow, &fonts);
    let saving = NoticeView::new(Tone::Recording, "Saving", "0:30…", true).saving(SavePhase::Saving);
    let saving = notice_scene(&saving, grow, &fonts);
    let sweep = Some(Sheen { progress: 0.5, color: ACTIVE });
    let now = std::time::Instant::now();
    let ring = Some(SaveMark::new(SavePhase::Saving, now).badge(now + std::time::Duration::from_millis(450)));
    let grown = Frame { w: stats.w + 40.0, h: 58.0, alpha: 1.0 };
    let cases = [
        ("stats pill", &stats, Frame { w: stats.w, h: stats.h, alpha: 1.0 }, 1.0, None, None),
        ("grown notice", &notice, grown, grow, None, None),
        ("grown notice + sweep", &notice, grown, grow, sweep, None),
        ("saving pill + spinning ring", &saving, grown, grow, None, ring),
    ];
    for (label, scene, frame, scale, sheen, badge) in cases {
        renderer.draw(&fonts, scene, Look::open(frame), scale, Layers { sheen, badge, ..Layers::default() }).unwrap();
        let start = std::time::Instant::now();
        for _ in 0..500 {
            renderer.draw(&fonts, scene, Look::open(frame), scale, Layers { sheen, badge, ..Layers::default() }).unwrap();
        }
        println!("draw_cost {label}: {:.0} us/frame", start.elapsed().as_secs_f64() * 1e6 / 500.0);
    }
}

#[test]
fn the_sweep_tints_the_pill_in_the_tone_colour_where_the_band_is_and_nowhere_else() {
    use crate::layout::{ACTIVE, ERROR};
    let (fonts, mut renderer) = (Fonts::new().unwrap(), Renderer::new().unwrap());
    let scene = Scene { w: 340.0, h: 58.0, ..Scene::default() };
    let frame = Frame { w: 340.0, h: 58.0, alpha: 1.0 };
    let pixel = |r: &Renderer, x: i32, y: i32| r.pixels(340, 58)[(y * 340 + x) as usize];
    renderer.draw(&fonts, &scene, Look::open(frame), 1.0, Layers::default()).unwrap();
    let plain = pixel(&renderer, 170, 29);
    let mid = Some(Sheen { progress: 0.5, color: ACTIVE });
    renderer.draw(&fonts, &scene, Look::open(frame), 1.0, Layers { sheen: mid, ..Layers::default() }).unwrap();
    let [b, g, r, _] = pixel(&renderer, 170, 29);
    assert!(g > plain[1] + 50 && g > r && g > b, "green band at the centre: {:?} vs {plain:?}", [b, g, r]);
    assert_eq!(pixel(&renderer, 12, 29), plain, "the pill's far end is untouched mid-sweep");
    assert_eq!(pixel(&renderer, 0, 0)[3], 0, "never outside the pill");
    let failure = Some(Sheen { progress: 0.5, color: ERROR });
    renderer.draw(&fonts, &scene, Look::open(frame), 1.0, Layers { sheen: failure, ..Layers::default() }).unwrap();
    let [b, g, r, _] = pixel(&renderer, 170, 29);
    assert!(r > g && r > b, "a failure sweeps in the error colour: {:?}", [b, g, r]);
    for progress in [0.0, 1.0] {
        renderer.draw(&fonts, &scene, Look::open(frame), 1.0, Layers { sheen: Some(Sheen { progress, color: ACTIVE }), ..Layers::default() }).unwrap();
        assert_eq!(pixel(&renderer, 170, 29), plain, "off the pill at {progress}");
        assert_eq!(pixel(&renderer, 6, 29), plain, "left end clear at {progress}");
        assert_eq!(pixel(&renderer, 333, 29), plain, "right end clear at {progress}");
    }
}

/// A side spot's stacked notice and its window, laid out with the real fonts.
fn side_notice(fonts: &Fonts, turn: Turn) -> (Scene, Look) {
    use crate::layout::notice_scene;
    use crate::notice::{NoticeView, Tone};
    let flat = notice_scene(&NoticeView::new(Tone::Recording, "REC", "1:23", true), 1.0, fonts);
    let tall = crate::stack::stack(&flat, 1.0, fonts);
    let look = Look { turn, align: 0.5, ..Look::open(Frame { w: tall.w, h: tall.h, alpha: 1.0 }) };
    (tall, look)
}

#[test]
fn stacked_cells_and_marks_draw_upright_with_no_rotation_or_mirror() {
    let (fonts, mut renderer) = (Fonts::new().unwrap(), Renderer::new().unwrap());
    for turn in [Turn::Right, Turn::Left] {
        let (scene, look) = side_notice(&fonts, turn);
        renderer.upright_log.borrow_mut().clear();
        let (w, h) = renderer.draw(&fonts, &scene, look, 1.0, Layers::default()).unwrap();
        let log = renderer.upright_log.borrow();
        assert_eq!(log.len(), scene.shapes.len() + scene.texts.len() + 1, "{turn:?}: every separator dot, cell and the mark drew upright");
        for m in log.iter() {
            assert!(m.M12 == 0.0 && m.M21 == 0.0 && m.M11 > 0.0 && (m.M11 - m.M22).abs() < 1e-6, "{turn:?}: no rotation, no mirror: {m:?}");
        }
        assert_eq!((w, h), (scene.h.ceil() as i32, scene.w.ceil() as i32), "{turn:?}: the window is the tall pill");
        // Every centre is mid-pill across (`scene.h / 2`): where it lands is M32 + that * k.
        let ys: Vec<f32> = log.iter().map(|m| m.M32 + scene.h / 2.0 * m.M22).collect();
        // Separators draw first, then the cells' text, the mark last: the mark is on top, the cells run downwards.
        let (mark, rest) = ys.split_last().unwrap();
        let cells = &rest[scene.shapes.len()..];
        assert!(cells.windows(2).all(|p| p[1] > p[0]) && mark < &cells[0], "{turn:?}: top to bottom: {ys:?}");
        assert!(ys.iter().all(|&y| y > 0.0 && y < h as f32), "{turn:?}: inside the window");
    }
}

/// Writes the REC notice (0:08, 1:23, 12:34, 1:02:03) and the stats pill,
/// horizontal and in the right side spot, to PNGs drawn offscreen (no
/// window) at 3x: `RECALL_SHOTS=<dir> cargo test -p recall-overlay -- --ignored cell_grid_pngs`.
#[test]
#[ignore]
fn cell_grid_pngs() {
    use crate::layout::{notice_scene, stats_scene, StatsContent};
    use crate::notice::{NoticeView, Tone};
    let Some(dir) = std::env::var_os("RECALL_SHOTS").map(std::path::PathBuf::from) else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let (fonts, mut renderer) = (Fonts::new().unwrap(), Renderer::new().unwrap());
    let k = 3.0;
    let st = StatsContent { recording: true, replay_secs: 8, fps: Some(144), low1: Some(98), gpu: Some(71), gpu_temp: Some(64), cpu: Some(23), ..Default::default() };
    let mut pills = vec![("stats".to_string(), stats_scene(&st, k, &fonts))];
    for timer in ["0:08", "1:23", "12:34", "1:02:03"] {
        let name = format!("rec-{}", timer.replace(':', "-"));
        pills.push((name, notice_scene(&NoticeView::new(Tone::Recording, "REC", timer, true), k, &fonts)));
    }
    for (what, flat) in &pills {
        let tall = crate::stack::stack(flat, k, &fonts);
        for (way, scene, turn) in [("horizontal", flat, Turn::None), ("vertical", &tall, Turn::Right)] {
            let look = Look { turn, align: 0.5, ..Look::open(Frame { w: scene.w, h: scene.h, alpha: 1.0 }) };
            let (w, h) = renderer.draw(&fonts, scene, look, k, Layers::default()).unwrap();
            // Over a mid-grey backdrop, unpremultiplied to RGBA.
            let pad = 36;
            let (cw, ch) = (w + 2 * pad, h + 2 * pad);
            let mut rgba = vec![0u8; (cw * ch * 4) as usize];
            for px in rgba.chunks_mut(4) {
                px.copy_from_slice(&[58, 62, 70, 255]);
            }
            for (i, [b, g, r, a]) in renderer.pixels(w, h).into_iter().enumerate() {
                let (x, y) = (i as i32 % w + pad, i as i32 / w + pad);
                let o = ((y * cw + x) * 4) as usize;
                let under = 1.0 - a as f32 / 255.0;
                for (c, src) in [r, g, b].into_iter().enumerate() {
                    rgba[o + c] = (src as f32 + rgba[o + c] as f32 * under).round().min(255.0) as u8;
                }
            }
            let path = dir.join(format!("{what}-{way}.png"));
            std::fs::write(&path, crate::png::encode(cw as u32, ch as u32, &rgba)).unwrap();
            println!("wrote {} ({w}x{h}, {} cells)", path.display(), scene.cells);
        }
    }
}

/// Three digits fit a number cell with the margin either side in the real
/// font, and a timer's two cells are the same width (tabular figures).
#[test]
fn three_digits_fit_a_number_cell_in_the_real_font() {
    use crate::cells::{Cells, MARGIN};
    use crate::layout::{Measure, Role};
    use crate::placement::ISLAND_H;
    let fonts = Fonts::new().unwrap();
    let cells = Cells::new(1.0, &fonts);
    for number in ["144", "888", "999", "000", "64°"] {
        assert!(cells.fits(Role::Fps, number), "{number}: {} px", fonts.measure(Role::Fps, 1.0, number).width);
        assert!(fonts.measure(Role::Fps, 1.0, number).width + 2.0 * MARGIN <= ISLAND_H);
    }
    let width = |t: &str| fonts.measure(Role::NoticeNumber, 1.0, t).width;
    assert!((width("00") - width("08")).abs() < 0.01 && (width("12") - width("34")).abs() < 0.01, "tabular timer cells");
    for word in ["REC", "GPU", "CPU"] {
        assert!(cells.fits(Role::NoticeTitle, word) && cells.fits(Role::Small, word), "{word}");
    }
}

/// `RECALL_NOTICES_PNG=path`: the island editor's hint and the fullscreen
/// notice at 1x, horizontal (prose whole) and vertical (stacked by word),
/// through the real renderer and fonts. No window.
#[test]
#[ignore]
fn notices_png() {
    use crate::layout::notice_scene;
    use crate::notice::{NoticeView, Tone};
    use crate::placement::{EDGE, REF_HEIGHT};
    let Some(path) = std::env::var_os("RECALL_NOTICES_PNG").map(std::path::PathBuf::from) else { return };
    let (fonts, mut renderer) = (Fonts::new().unwrap(), Renderer::new().unwrap());
    let notices = [
        NoticeView::new(Tone::Neutral, crate::app::editor::HINT_TITLE, crate::app::editor::HINT_DETAIL, false),
        NoticeView::new(Tone::Failure, "Island hidden by fullscreen", "Rocket League runs in exclusive fullscreen.", false),
    ];
    let mut tiles: Vec<(i32, i32, Vec<[u8; 4]>)> = Vec::new();
    for turn in [Turn::None, Turn::Right] {
        for notice in &notices {
            let flat = notice_scene(notice, 1.0, &fonts);
            let scene = match turn {
                Turn::None => flat,
                _ => crate::stack::stack_within(&flat, 1.0, &fonts, REF_HEIGHT - 2.0 * EDGE),
            };
            let look = Look { turn, align: 0.5, ..Look::open(Frame { w: scene.w, h: scene.h, alpha: 1.0 }) };
            let (w, h) = renderer.draw(&fonts, &scene, look, 1.0, Layers::default()).unwrap();
            tiles.push((w, h, renderer.pixels(w, h)));
        }
    }
    // Horizontal pills stacked on the left, the side pills in columns to their right.
    let pad = 32;
    let left_w = tiles[..2].iter().map(|t| t.0).max().unwrap();
    let cw = pad + left_w + tiles[2..].iter().map(|t| pad + t.0).sum::<i32>() + pad;
    let ch = pad + tiles[2..].iter().map(|t| t.1).max().unwrap().max(tiles[0].1 + tiles[1].1 + pad) + pad;
    let mut rgba: Vec<u8> = [58u8, 62, 70, 255].repeat((cw * ch) as usize);
    let mut place = |(w, h, px): &(i32, i32, Vec<[u8; 4]>), ox: i32, oy: i32| {
        for (i, [b, g, r, a]) in px.iter().copied().enumerate() {
            let (x, y) = (ox + i as i32 % w, oy + i as i32 / w);
            let o = ((y * cw + x) * 4) as usize;
            let under = 1.0 - a as f32 / 255.0;
            for (c, src) in [r, g, b].into_iter().enumerate() {
                rgba[o + c] = (src as f32 + rgba[o + c] as f32 * under).round().min(255.0) as u8;
            }
        }
        let _ = h;
    };
    place(&tiles[0], pad, pad);
    place(&tiles[1], pad, pad * 2 + tiles[0].1);
    let mut x = pad + left_w + pad;
    for tile in &tiles[2..] {
        place(tile, x, pad);
        x += tile.0 + pad;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(&path, crate::png::encode(cw as u32, ch as u32, &rgba)).unwrap();
    println!("wrote {} ({cw}x{ch})", path.display());
}
