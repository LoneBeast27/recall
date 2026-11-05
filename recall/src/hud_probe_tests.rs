use super::*;

/// A cheap deterministic hash noise in 0..255.
fn noise(x: usize, y: usize) -> f32 {
    let mut v = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263);
    v = (v ^ (v >> 13)).wrapping_mul(1_274_126_177);
    ((v ^ (v >> 16)) & 0xff) as f32
}

/// A 640x360 frame: a textured world scrolled by `shift` px, with a static
/// striped HUD panel (text-like edges) over `hud` = (x0, y0, x1, y1).
fn frame(shift: usize, hud: Option<(usize, usize, usize, usize)>) -> Gray {
    let (w, h) = (640, 360);
    let px = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            match hud {
                Some((x0, y0, x1, y1)) if (x0..x1).contains(&x) && (y0..y1).contains(&y) => {
                    if (x / 2) % 2 == 0 { 230.0 } else { 20.0 }
                }
                _ => noise((x + shift) / 3, y / 3),
            }
        })
        .collect();
    Gray { w, h, px }
}

fn burst(start: usize, hud: Option<(usize, usize, usize, usize)>) -> Vec<Gray> {
    (0..3).map(|k| frame(start + k * 17, hud)).collect()
}

/// Bit c of `row` for every (c, r) cell over the analysis px rect.
fn cells(grid: &[u64]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (r, bits) in grid.iter().enumerate() {
        for c in 0..GRID_COLS {
            if bits >> c & 1 == 1 {
                out.push((c, r));
            }
        }
    }
    out
}

#[test]
fn a_still_world_says_nothing() {
    let still: Vec<Gray> = (0..3).map(|_| frame(0, Some((0, 0, 100, 40)))).collect();
    assert_eq!(steady_edges(&still), None);
    let mut tally = Tally::default();
    tally.add(&still);
    assert_eq!((tally.moving, tally.bursts), (0, 1));
}

#[test]
fn static_hud_over_a_moving_world_lands_in_its_cells_only() {
    // A panel at analysis px x 20-120, y 300-340 (bottom-left), 10 px per cell.
    let hud = Some((20, 300, 120, 340));
    let mut tally = Tally::default();
    for b in 0..6 {
        tally.add(&burst(b * 101, hud));
    }
    assert_eq!(tally.moving, 6);
    let grid = tally.grid();
    assert_eq!(grid.len(), GRID_ROWS);
    let found = cells(&grid);
    assert!(!found.is_empty(), "the panel is found");
    for (c, r) in &found {
        // The cells the panel touches (allowing the 9x9 density box and 3x3 tidy at its border).
        assert!((1..=12).contains(c) && (29..=34).contains(r), "cell {c},{r} is outside the panel");
    }
    assert!(found.contains(&(6, 32)), "the panel's middle cell: {found:?}");
}

#[test]
fn hud_seen_in_one_burst_only_is_not_counted() {
    let mut tally = Tally::default();
    tally.add(&burst(0, Some((500, 10, 620, 60))));
    for b in 1..6 {
        tally.add(&burst(b * 101, None));
    }
    assert_eq!(tally.moving, 6);
    assert!(cells(&tally.grid()).is_empty(), "one sighting is a fluke, two count");
}

#[test]
fn hud_that_comes_and_goes_counts() {
    let mut tally = Tally::default();
    for b in 0..8 {
        let feed = (b % 3 == 0).then_some((500, 10, 620, 60));
        tally.add(&burst(b * 101, feed));
    }
    let found = cells(&tally.grid());
    assert!(found.contains(&(56, 3)), "a kill feed up for 3 of 8 bursts: {found:?}");
}

#[test]
fn a_size_change_starts_the_tally_over() {
    let mut tally = Tally::default();
    tally.add(&burst(0, None));
    let small: Vec<Gray> = burst(5, None).into_iter().map(|g| Gray { w: 320, h: 720, px: g.px }).collect();
    tally.add(&small);
    assert_eq!((tally.moving, tally.bursts, tally.w), (1, 2, 320));
}

#[test]
fn bgra_frames_box_average_to_about_640_wide() {
    // 1920x1080 BGRA with a white 3x3 block at (3, 3): one analysis pixel at (1, 1).
    let (w, h) = (1920usize, 1080usize);
    let mut src = vec![0u8; w * h * 4];
    for y in 3..6 {
        for x in 3..6 {
            src[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    let g = Gray::from_bgra(&src, w * 4, (0, 0, w, h));
    assert_eq!((g.w, g.h), (640, 360));
    assert!((g.px[640 + 1] - 255.0).abs() < 0.5);
    assert_eq!(g.px[0], 0.0);
    // 2560x1440 → /4, a crop keeps its own size.
    let src = vec![128u8; 2560 * 1440 * 4];
    let g = Gray::from_bgra(&src, 2560 * 4, (0, 0, 2560, 1440));
    assert_eq!((g.w, g.h), (640, 360));
    let g = Gray::from_bgra(&src, 2560 * 4, (320, 0, 1920, 1080));
    assert_eq!((g.w, g.h), (640, 360));
    assert!((g.px[0] - 128.0).abs() < 0.5);
}

#[test]
fn the_scan_line_is_what_the_island_reads() {
    let scan = Outbound::HudScan { ok: true, cols: 64, rows: 36, grid: vec![1, 2], moving: 8, bursts: 9, reason: None };
    let wire = serde_json::to_value(&scan).unwrap();
    assert_eq!(wire["type"], "hud_scan");
    assert_eq!(wire["grid"], serde_json::json!([1, 2]));
    assert_eq!((wire["cols"].as_u64(), wire["rows"].as_u64()), (Some(64), Some(36)));
}
