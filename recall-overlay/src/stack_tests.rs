use super::*;
use crate::layout::{notice_scene, stats_scene, StatsContent, TextMetrics};
use crate::notice::{NoticeView, Tone};
use crate::placement::REF_HEIGHT;

struct Fake;
impl Measure for Fake {
    fn measure(&self, role: Role, scale: f32, text: &str) -> TextMetrics {
        let em = role.size() * scale;
        TextMetrics { width: text.chars().count() as f32 * em * 0.6, height: em * 1.25, baseline: em * 0.95, cap: em * 0.7 }
    }
}

fn glyphs(scene: &Scene) -> Vec<&str> {
    scene.texts.iter().filter(|t| t.role != Role::RingNumber).map(|t| t.text.as_str()).collect()
}

#[test]
fn every_run_becomes_one_upright_glyph_per_line_top_to_bottom() {
    let notice = NoticeView::new(Tone::Recording, "REC", "1:23", true);
    let flat = notice_scene(&notice, 1.0, &Fake);
    let tall = stack(&flat, 1.0, &Fake);
    assert!(tall.stacked && !flat.stacked);
    // The line count equals the character count: R, E, C, then 1, :, 2, 3.
    assert_eq!(glyphs(&tall), ["R", "E", "C", "1", ":", "2", "3"]);
    // Each glyph is centred across the pill and strictly below the one before.
    assert!(tall.texts.iter().all(|t| (t.y - tall.h / 2.0).abs() < 1e-4), "centred across");
    assert!(tall.texts.windows(2).all(|w| w[1].x > w[0].x), "one per line, in reading order");
    // The mark is at the top, before the first glyph.
    let icon = tall.icon.expect("the mark stays");
    assert!(icon.cx < tall.texts[0].x && (icon.cy - tall.h / 2.0).abs() < 1e-4);
    // The pill keeps its thickness and grows along to hold every line.
    assert_eq!(tall.h, flat.h);
    assert!(tall.w >= tall.texts.last().unwrap().x);
}

#[test]
fn a_space_is_a_small_gap_not_a_line() {
    let flat = notice_scene(&NoticeView::new(Tone::Success, "Clip saved", "", false), 1.0, &Fake);
    let tall = stack(&flat, 1.0, &Fake);
    assert_eq!(glyphs(&tall).concat(), "Clipsaved");
    let step = |i: usize| tall.texts[i + 1].x - tall.texts[i].x;
    assert!(step(3) > step(2) && step(3) < 2.0 * step(2), "the space between p and s is a short gap");
}

#[test]
fn stats_stack_with_rings_kept_whole_and_their_numbers_upright_inside() {
    let st = StatsContent { recording: true, replay_secs: 120, fps: Some(144), low1: Some(98), gpu: Some(71), gpu_temp: Some(64), cpu: Some(23), ..Default::default() };
    let flat = stats_scene(&st, 1.0, &Fake);
    let tall = stack(&flat, 1.0, &Fake);
    assert_eq!(glyphs(&tall).concat(), "2:001441%low98GPU64°CCPU");
    assert_eq!(tall.shapes.len(), 2, "both rings");
    for (shape, number) in tall.shapes.iter().zip(["71", "23"]) {
        let Shape::Ring { cx, cy, .. } = *shape else { panic!("a ring") };
        let text = tall.texts.iter().find(|t| t.text == number).expect("the ring's number");
        assert_eq!((text.x, text.y), (cx, cy), "{number} sits whole at its ring's centre");
    }
    assert!(tall.w <= REF_HEIGHT - 2.0 * crate::placement::EDGE, "the stats fit down a 1440p edge: {}", tall.w);
}

#[test]
fn the_longest_notice_still_fits_down_the_screen() {
    let title = "W".repeat(32);
    let detail = "W".repeat(40);
    let flat = notice_scene(&NoticeView::new(Tone::Neutral, &title, detail, false), 1.0, &Fake);
    let tall = stack(&flat, 1.0, &Fake);
    assert_eq!(glyphs(&tall).len(), 72);
    assert!(tall.w <= REF_HEIGHT - 2.0 * crate::placement::EDGE, "72 glyphs fit: {}", tall.w);
}

/// A notice longer than the side's room stops with "…" inside it, never off the monitor.
#[test]
fn a_column_longer_than_the_side_stops_with_an_ellipsis_inside_it() {
    let notice = NoticeView::new(Tone::Failure, "Island hidden by fullscreen", "Rocket League runs in exclusive fullscreen.", false);
    let flat = notice_scene(&notice, 1.0, &Fake);
    let free = stack(&flat, 1.0, &Fake);
    let max = 300.0;
    assert!(free.w > max, "the whole notice would be {} px", free.w);
    let tall = stack_within(&flat, 1.0, &Fake, max);
    assert!(tall.w <= max, "{} px fits in {max}", tall.w);
    assert_eq!(glyphs(&tall).last().copied(), Some("…"));
    assert_eq!(&glyphs(&tall)[..3], ["I", "s", "l"], "it starts as before");
    let short = NoticeView::new(Tone::Recording, "REC", "1:23", true);
    let flat = notice_scene(&short, 1.0, &Fake);
    assert_eq!(glyphs(&stack_within(&flat, 1.0, &Fake, max)), glyphs(&stack(&flat, 1.0, &Fake)), "a short one is untouched");
}
