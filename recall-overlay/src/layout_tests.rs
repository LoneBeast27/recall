use super::*;

/// Monospace-ish fake: every char is 0.6 em wide, box 1.25 em, baseline 0.95 em, cap 0.7 em.
struct Fake;

impl Measure for Fake {
    fn measure(&self, role: Role, scale: f32, text: &str) -> TextMetrics {
        let em = role.size() * scale;
        TextMetrics { width: text.chars().count() as f32 * em * 0.6, height: em * 1.25, baseline: em * 0.95, cap: em * 0.7 }
    }
}

fn stats() -> StatsContent {
    StatsContent {
        recording: true,
        replay_secs: 120,
        fps: Some(144),
        low1: Some(98),
        gpu: Some(71),
        gpu_temp: Some(64),
        cpu: Some(23),
        ..StatsContent::default()
    }
}

fn text<'a>(scene: &'a Scene, needle: &str) -> &'a Text {
    scene.texts.iter().find(|t| t.text == needle).unwrap_or_else(|| panic!("no text {needle:?} in {:?}", scene.texts))
}

fn inside(scene: &Scene) -> bool {
    let texts_in = scene.texts.iter().all(|t| t.x >= 0.0 && t.y >= 0.0 && t.y + t.role.size() * 1.25 <= scene.h + 0.01);
    let shapes_in = scene.shapes.iter().all(|shape| match *shape {
        Shape::Dot { cx, cy, r, .. } | Shape::Hollow { cx, cy, r, .. } => cx - r >= 0.0 && cy + r <= scene.h,
        Shape::Ring { cx, cy, r, stroke, .. } => cx + r + stroke / 2.0 <= scene.w && cy + r + stroke / 2.0 <= scene.h,
        Shape::Pause { x, .. } => x >= 0.0,
    });
    let icon_in = scene.icon.is_none_or(|i| i.cx - i.r >= 0.0 && i.cy + i.r <= scene.h);
    texts_in && shapes_in && icon_in
}

fn icon(scene: &Scene) -> Shape {
    scene.icon.expect("a leading mark").shape
}

#[test]
fn notice_is_44px_tall_at_1440p_and_scales_with_the_monitor() {
    let notice = NoticeView::new(Tone::Recording, "Recording", "VALORANT", true);
    let at_1440 = notice_scene(&notice, 1.0, &Fake);
    let at_2160 = notice_scene(&notice, 1.5, &Fake);
    assert_eq!(at_1440.h, 44.0);
    assert_eq!(at_2160.h, 66.0);
    assert!((at_2160.w - at_1440.w * 1.5).abs() <= 1.0, "{} vs {}", at_2160.w, at_1440.w);
    assert!(matches!(icon(&at_1440), Shape::Dot { color: SIGNAL, .. }), "red record dot");
    assert!(inside(&at_1440) && inside(&at_2160));
}

#[test]
fn notice_puts_the_small_title_over_the_bold_game_name() {
    let notice = NoticeView::new(Tone::Recording, "Recording", "VALORANT", true);
    let scene = notice_scene(&notice, 1.0, &Fake);
    let (title, game) = (text(&scene, "Recording"), text(&scene, "VALORANT"));
    assert_eq!((title.role, title.color), (Role::NoticeTitle, MUTED));
    assert_eq!((game.role, game.color), (Role::NoticeGame, TEXT));
    assert!(title.y < game.y);
    assert_eq!(title.x, game.x);
    // The caps of both lines sit centred in the pill.
    let top = title.y + 11.0 * 0.95 - 11.0 * 0.7;
    let bottom = game.y + 15.0 * 0.95;
    assert!((top - (44.0 - bottom)).abs() < 0.01, "top {top} bottom gap {}", 44.0 - bottom);
    // Width = text start + widest line + right padding.
    assert_eq!(scene.w, (34.0 + 8.0 * 15.0 * 0.6 + 20.0f32).ceil());
}

#[test]
fn non_game_details_use_the_ui_face_and_one_liners_centre() {
    let saved = NoticeView::new(Tone::Failure, "Save failed", "disk full", false);
    assert_eq!(text(&notice_scene(&saved, 1.0, &Fake), "disk full").role, Role::NoticeUi);
    let waiting = NoticeView::new(Tone::Waiting, "Waiting for a game", "", true);
    let scene = notice_scene(&waiting, 1.0, &Fake);
    assert_eq!(scene.texts.len(), 1);
    let line = &scene.texts[0];
    let cap_mid = line.y + 14.0 * 0.95 - 14.0 * 0.7 / 2.0;
    assert!((cap_mid - 22.0).abs() < 0.01);
    assert!(matches!(icon(&scene), Shape::Hollow { .. }));
    let paused = NoticeView::new(Tone::Paused, "Recording paused", "VALORANT", true);
    assert!(matches!(icon(&notice_scene(&paused, 1.0, &Fake)), Shape::Pause { .. }));
}

#[test]
fn stats_scene_shows_replay_fps_low_and_two_rings() {
    let scene = stats_scene(&stats(), 1.0, &Fake);
    assert_eq!(scene.h, 44.0);
    for needle in ["2:00", "144", "1% low 98", "71", "GPU", "64°C", "23", "CPU"] {
        text(&scene, needle);
    }
    let rings: Vec<_> = scene.shapes.iter().filter(|s| matches!(s, Shape::Ring { .. })).collect();
    assert_eq!(rings.len(), 2);
    assert!(matches!(rings[0], Shape::Ring { frac, color: TEXT, .. } if (*frac - 0.71).abs() < 1e-6));
    assert!(matches!(icon(&scene), Shape::Dot { color: SIGNAL, .. }));
    assert!(text(&scene, "144").x < text(&scene, "71").x && text(&scene, "71").x < text(&scene, "23").x);
    assert!(inside(&scene));
}

#[test]
fn stats_width_holds_still_as_numbers_change() {
    let base = stats_scene(&stats(), 1.0, &Fake).w;
    let busier = StatsContent { fps: Some(97), low1: Some(61), gpu: Some(100), cpu: Some(5), gpu_temp: Some(9), ..stats() };
    assert_eq!(stats_scene(&busier, 1.0, &Fake).w, base);
}

#[test]
fn hot_gpu_turns_amber_and_missing_sources_degrade() {
    let hot = stats_scene(&StatsContent { gpu_temp: Some(HOT_GPU_C), ..stats() }, 1.0, &Fake);
    assert_eq!(text(&hot, "83°C").color, WARNING);
    assert!(hot.shapes.iter().any(|s| matches!(s, Shape::Ring { color: WARNING, .. })));
    let cool = stats_scene(&StatsContent { gpu_temp: Some(HOT_GPU_C - 1), ..stats() }, 1.0, &Fake);
    assert_eq!(text(&cool, "82°C").color, TEXT);

    let bare = StatsContent { fps: None, low1: None, fps_note: Some("No frame timing".into()), gpu: None, cpu: None, ..stats() };
    let scene = stats_scene(&bare, 1.0, &Fake);
    text(&scene, "No frame timing");
    assert_eq!(scene.texts.iter().filter(|t| t.text == "—").count(), 2, "FPS and the CPU ring show dashes");
    assert!(!scene.texts.iter().any(|t| t.text == "GPU"), "no NVML → no GPU ring");
    assert_eq!(scene.shapes.iter().filter(|s| matches!(s, Shape::Ring { .. })).count(), 1);
    assert!(scene.w < stats_scene(&stats(), 1.0, &Fake).w);
}

#[test]
fn paused_and_idle_marks() {
    let paused = stats_scene(&StatsContent { paused: true, recording: false, ..stats() }, 1.0, &Fake);
    assert!(matches!(icon(&paused), Shape::Pause { .. }));
    assert_eq!(text(&paused, "2:00").color, MUTED);
    let idle = stats_scene(&StatsContent { recording: false, ..stats() }, 1.0, &Fake);
    assert!(matches!(icon(&idle), Shape::Hollow { .. }));
}

#[test]
fn a_clip_save_carries_the_badge_and_steps_its_text_clear_of_the_ring() {
    let plain = NoticeView::new(Tone::Success, "Clip saved", "VALORANT · 0:30", true);
    let saving = NoticeView::new(Tone::Recording, "Saving", "0:30…", true).saving(SavePhase::Saving);
    let (a, b) = (notice_scene(&plain, 1.0, &Fake), notice_scene(&saving, 1.0, &Fake));
    assert_eq!(a.icon.unwrap().badge, None);
    let badge = b.icon.unwrap();
    assert_eq!((badge.badge, badge.cx, badge.cy, badge.r), (Some(SavePhase::Saving), 20.0, 22.0, 4.0));
    let text_x = text(&b, "Saving").x;
    assert_eq!(text_x, text(&a, "Clip saved").x + 4.0);
    assert!(text_x - (badge.cx + 2.0 * badge.r + 1.0) >= 8.0, "8+ px between the ring's outer edge and the text");
    assert!(inside(&b));
}

#[test]
fn clock_formats_the_replay_length() {
    assert_eq!(clock(120), "2:00");
    assert_eq!(clock(59), "0:59");
    assert_eq!(clock(3_725), "1:02:05");
}

#[test]
fn ring_sweep_runs_clockwise_from_twelve() {
    assert_eq!(sweep(15.0, 15.0, 10.0, 0.0), Sweep::Empty);
    assert_eq!(sweep(15.0, 15.0, 10.0, 1.0), Sweep::Full);
    let close = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4;
    let Sweep::Arc { start, end, large } = sweep(15.0, 15.0, 10.0, 0.25) else { panic!() };
    assert!(close(start, (15.0, 5.0)) && close(end, (25.0, 15.0)) && !large, "quarter ends at 3 o'clock");
    let Sweep::Arc { end, large, .. } = sweep(15.0, 15.0, 10.0, 0.5 + 1e-3) else { panic!() };
    assert!(large && end.0 < 15.0 + 0.1, "just past half needs the large arc");
    let Sweep::Arc { end, .. } = sweep(15.0, 15.0, 10.0, 0.75) else { panic!() };
    assert!(close(end, (5.0, 15.0)), "three quarters ends at 9 o'clock");
}
