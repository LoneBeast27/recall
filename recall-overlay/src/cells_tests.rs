use super::*;
use crate::layout::{notice_scene, stats_scene, StatsContent, TextMetrics};
use crate::notice::NoticeView;
use crate::placement::{EDGE, REF_HEIGHT};
use crate::stack::{stack, stack_within};

/// Monospace-ish fake: every char is 0.6 em wide, box 1.25 em, baseline 0.95 em, cap 0.7 em.
struct Fake;
impl Measure for Fake {
    fn measure(&self, role: Role, scale: f32, text: &str) -> TextMetrics {
        let em = role.size() * scale;
        TextMetrics { width: text.chars().count() as f32 * em * 0.6, height: em * 1.25, baseline: em * 0.95, cap: em * 0.7 }
    }
}

fn rec(timer: &str) -> NoticeView {
    NoticeView::new(Tone::Recording, "REC", timer, true)
}

fn stats() -> StatsContent {
    StatsContent { recording: true, replay_secs: 8, fps: Some(144), low1: Some(98), gpu: Some(71), gpu_temp: Some(64), cpu: Some(23), ..Default::default() }
}

/// Each text's centre (`x` along, `y` across), for either orientation.
fn centres(scene: &Scene, s: f32) -> Vec<(String, f32, f32)> {
    scene
        .texts
        .iter()
        .map(|t| {
            if scene.stacked {
                return (t.text.clone(), t.x, t.y);
            }
            let m = Fake.measure(t.role, s, &t.text);
            (t.text.clone(), t.x + m.width / 2.0, t.y + m.baseline - m.cap / 2.0)
        })
        .collect()
}

fn words(scene: &Scene) -> Vec<String> {
    scene.texts.iter().map(|t| t.text.clone()).collect()
}

#[test]
fn the_rec_timer_is_two_square_cells_minutes_then_seconds() {
    for (timer, want) in [("0:08", ["REC", "00", "08"].as_slice()), ("1:23", &["REC", "01", "23"]), ("12:34", &["REC", "12", "34"]), ("1:02:03", &["REC", "1", "02", "03"])] {
        let flat = notice_scene(&rec(timer), 1.0, &Fake);
        assert_eq!(words(&flat), want, "{timer}");
        assert_eq!(flat.cells, want.len() + 1, "{timer}: the mark + one cell each");
        // A dim colon (two dots across the pill) between each pair of timer parts.
        let colons = flat.shapes.iter().filter(|s| matches!(s, Shape::Dot { .. })).count();
        assert_eq!(colons, 2 * (want.len() - 2), "{timer}");
    }
}

#[test]
fn every_cell_is_a_square_the_pills_thickness() {
    for s in [1.0, 1.5] {
        for scene in [notice_scene(&rec("12:34"), s, &Fake), stats_scene(&stats(), s, &Fake)] {
            let side = ISLAND_H * s;
            assert_eq!(scene.h, side);
            assert!((scene.pitch - (side + GAP * s)).abs() < 1e-4);
            assert!((scene.w - length(scene.cells, s).ceil()).abs() < 1e-3, "length = cells × S + gaps");
            // Every content centre sits at a cell's centre, both ways.
            for (text, x, y) in centres(&scene, s) {
                let i = (x / scene.pitch).floor();
                assert!((x - (i * scene.pitch + side / 2.0)).abs() < 1e-3, "{text} centred along its cell");
                assert!((y - side / 2.0).abs() < 8.0 * s, "{text} inside its cell across");
            }
        }
    }
}

#[test]
fn horizontal_and_vertical_have_the_same_cells_in_the_same_order() {
    for flat in [notice_scene(&rec("0:08"), 1.0, &Fake), notice_scene(&rec("12:34"), 1.0, &Fake), stats_scene(&stats(), 1.0, &Fake)] {
        let tall = stack(&flat, 1.0, &Fake);
        assert!(tall.stacked && !flat.stacked);
        assert_eq!((tall.cells, tall.w, tall.h), (flat.cells, flat.w, flat.h), "same cells, same length, same thickness");
        assert_eq!(words(&tall), words(&flat));
        // Each cell's content turns back upright inside its cell: an offset
        // across the horizontal cell ("1%" above "low") runs down the tall one.
        for ((a, ax, ay), (b, bx, by)) in centres(&flat, 1.0).into_iter().zip(centres(&tall, 1.0)) {
            assert_eq!(a, b);
            let mid = (ax / flat.pitch).floor() * flat.pitch + flat.h / 2.0;
            let (tx, ty) = (mid + (ay - flat.h / 2.0), flat.h / 2.0 - (ax - mid));
            assert!((tx - bx).abs() < 1e-3 && (ty - by).abs() < 1e-3, "{a}: ({bx}, {by}), want ({tx}, {ty})");
            assert!((bx / flat.pitch).floor() == (ax / flat.pitch).floor(), "{a}: the same cell in both");
        }
        let low = |s: &Scene| s.texts.iter().position(|t| t.text == "low");
        if let (Some(i), Some(j)) = (low(&flat), flat.texts.iter().position(|t| t.text == "1%")) {
            assert!(tall.texts[j].x < tall.texts[i].x && (tall.texts[j].y - tall.texts[i].y).abs() < 1e-3, "1% sits above low in the tall pill too");
        }
        assert_eq!(tall.shapes, flat.shapes, "rings and separators stay");
        assert_eq!(tall.icon, flat.icon, "the mark stays in the first cell");
    }
}

#[test]
fn stats_cells_and_their_order() {
    let flat = stats_scene(&stats(), 1.0, &Fake);
    assert_eq!(words(&flat), ["00", "08", "144", "1%", "low", "98", "71", "GPU", "64°", "23", "CPU"]);
    assert_eq!(flat.cells, 11, "mark, 2 timer, fps, 1% low, 98, ring, GPU, 64°, ring, CPU");
    // Every number cell is the same size.
    let numbers: Vec<_> = flat.texts.iter().filter(|t| ["00", "08", "144", "98", "64°"].contains(&t.text.as_str())).map(|t| t.role).collect();
    assert!(numbers.iter().all(|&r| r == Role::Fps), "{numbers:?}");
    // The cell count holds still as numbers change.
    let busier = StatsContent { replay_secs: 754, fps: Some(97), low1: Some(6), gpu: Some(100), cpu: Some(5), gpu_temp: Some(9), ..stats() };
    assert_eq!(stats_scene(&busier, 1.0, &Fake).cells, flat.cells);
    for scene in [&flat, &stack(&flat, 1.0, &Fake)] {
        assert!(scene.w <= REF_HEIGHT - 2.0 * EDGE, "fits down a 1440p edge: {}", scene.w);
    }
}

#[test]
fn three_digits_fit_a_number_cell() {
    let cells = Cells::new(1.0, &Fake);
    assert!(cells.fits(Role::Fps, "888") && cells.fits(Role::NoticeNumber, "000"));
}

#[test]
fn the_longest_notice_fits_a_1440p_edge_both_ways() {
    let (title, detail) = ("W".repeat(32), "W".repeat(40));
    let flat = notice_scene(&NoticeView::new(Tone::Neutral, &title, detail.clone(), false), 1.0, &Fake);
    assert_eq!(words(&flat), [title.clone(), detail], "horizontal: each run whole");
    let tall = stack(&flat, 1.0, &Fake);
    assert_eq!(words(&tall).concat().chars().count(), 72, "vertical: every glyph has a cell");
    assert!(words(&tall).iter().all(|g| g.chars().count() <= 3), "3 glyphs a cell at most");
    for scene in [&flat, &tall] {
        assert!(scene.w <= REF_HEIGHT - 2.0 * EDGE, "72 glyphs fit: {}", scene.w);
    }
}

/// A notice longer than the side's room ends in a "…" cell, never off the monitor.
#[test]
fn a_column_longer_than_the_side_ends_in_an_ellipsis_cell() {
    let notice = NoticeView::new(Tone::Failure, "Island hidden by fullscreen", "Rocket League runs in exclusive fullscreen.", false);
    let flat = notice_scene(&notice, 1.0, &Fake);
    let max = 300.0;
    assert!(flat.w > max, "the whole notice would be {} px", flat.w);
    let tall = stack_within(&flat, 1.0, &Fake, max);
    assert!(tall.w <= max, "{} px fits in {max}", tall.w);
    assert_eq!(tall.cells, 6, "300 px holds 6 cells");
    assert_eq!(words(&tall).last().map(String::as_str), Some("…"));
    assert_eq!(&words(&tall)[..2], ["Isl", "and"], "it starts as before");
    assert_eq!(stack_within(&notice_scene(&rec("1:23"), 1.0, &Fake), 1.0, &Fake, max), stack(&notice_scene(&rec("1:23"), 1.0, &Fake), 1.0, &Fake), "a short one is untouched");
}

#[test]
fn a_saves_stages_keep_the_detail_cells_put() {
    let stage = |tone, title: &str, phase| notice_scene(&NoticeView::new(tone, title, "VALORANT · 0:30", true).saving(phase), 1.3, &Fake);
    let (saving, saved) = (stage(Tone::Recording, "Saving", SavePhase::Saving), stage(Tone::Success, "Clip saved", SavePhase::Saved));
    assert_eq!(saving.w, saved.w, "one length for both stages");
    let at = |scene: &Scene, needle: &str| scene.texts.iter().find(|t| t.text == needle).map(|t| (t.x, t.y));
    assert_eq!(at(&saving, "VALORANT · 0:30"), at(&saved, "VALORANT · 0:30"), "the detail stays put");
    assert!(saving.lead && saved.lead);
    assert_eq!(saving.icon.unwrap().badge, Some(SavePhase::Saving));
    // The side pill: the same cell count for both stages, the detail's cells put.
    let (a, b) = (stack(&saving, 1.3, &Fake), stack(&saved, 1.3, &Fake));
    assert_eq!((a.w, a.cells), (b.w, b.cells));
    for cell in ["VAL", "00", "30"] {
        assert_eq!(at(&a, cell), at(&b, cell), "{cell} stays put down the side");
    }
    // "VALORANT · 0:30" in cells: a dot in the gap before the timer, a colon inside it.
    assert_eq!(b.shapes.iter().filter(|s| matches!(s, Shape::Dot { .. })).count(), 3);
    assert!(saved.shapes.is_empty(), "horizontal: the run carries its own dot and colon");
}

/// Rohan: prose is never chopped into 3-letter cells. A horizontal pill
/// draws each sentence as one run; only compact fields take square cells.
#[test]
fn horizontal_prose_is_one_run_per_sentence_with_no_cell_splits() {
    use crate::cells::is_prose;
    let hint = NoticeView::new(Tone::Neutral, "Island editor", "Drag to place · right-click cancel · ⟲ auto", false);
    let fullscreen = NoticeView::new(Tone::Failure, "Island hidden by fullscreen", "Rocket League runs in exclusive fullscreen.", false);
    for notice in [&hint, &fullscreen] {
        let flat = notice_scene(notice, 1.0, &Fake);
        assert_eq!(words(&flat), [notice.title.clone(), notice.detail.clone()], "two runs, whole");
        assert!(flat.tall.is_some(), "the cells are kept for the side pill");
        let [title, detail] = &flat.texts[..] else { panic!() };
        assert!(title.x + Fake.measure(title.role, 1.0, &title.text).width < detail.x, "title, then detail, apart");
        let end = detail.x + Fake.measure(detail.role, 1.0, &detail.text).width;
        assert!((flat.w - end - 20.0).abs() <= 1.0, "20 px after the last run: {} vs {end}", flat.w);
    }
    let saved = notice_scene(&NoticeView::new(Tone::Success, "Clip saved", "VALORANT", true), 1.0, &Fake);
    assert_eq!(words(&saved), ["Clip saved", "VALORANT"], "a game name is prose too");
    // Compact fields stay square cells.
    assert_eq!(words(&notice_scene(&rec("1:23"), 1.0, &Fake)), ["REC", "01", "23"]);
    assert!(notice_scene(&rec("1:23"), 1.0, &Fake).tall.is_none());
    for compact in ["REC", "64°", "0:30…", "144", "x"] {
        assert!(!is_prose(compact), "{compact}");
    }
    for prose in ["Saving", "disk full", "No frame timing", "VALORANT · 0:30"] {
        assert!(is_prose(prose), "{prose}");
    }
}

/// The side pill keeps the stacking rule: whole words, upright, up to 3
/// glyphs a line, a 1-3 glyph word whole; long prose caps with "…".
#[test]
fn vertical_prose_stacks_by_word() {
    let hint = NoticeView::new(Tone::Neutral, "Island editor", "Drag to place · right-click cancel · ⟲ auto", false);
    let tall = stack(&notice_scene(&hint, 1.0, &Fake), 1.0, &Fake);
    assert!(tall.stacked);
    assert_eq!(
        words(&tall),
        ["Isl", "and", "edi", "tor", "Dr", "ag", "to", "pla", "ce", "rig", "ht-", "cli", "ck", "can", "cel", "⟲", "au", "to"]
    );
    assert!(words(&tall).iter().all(|w| w.chars().count() <= 3));
    let capped = stack_within(&notice_scene(&hint, 1.0, &Fake), 1.0, &Fake, 400.0);
    assert!(capped.w <= 400.0 && words(&capped).last().map(String::as_str) == Some("…"));
}
