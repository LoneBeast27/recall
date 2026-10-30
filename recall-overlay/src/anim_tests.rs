use super::*;

const A: Frame = Frame { w: 100.0, h: 44.0, alpha: 1.0 };
const B: Frame = Frame { w: 300.0, h: 44.0, alpha: 1.0 };

#[test]
fn ease_out_is_monotonic_and_front_loaded() {
    assert_eq!((ease_out(0.0), ease_out(1.0), ease_out(2.0)), (0.0, 1.0, 1.0));
    assert!(ease_out(0.5) > 0.5, "ease-out covers most of the distance early");
    let steps: Vec<f32> = (0..=10).map(|i| ease_out(i as f32 / 10.0)).collect();
    assert!(steps.windows(2).all(|pair| pair[1] >= pair[0]));
}

#[test]
fn tween_runs_from_start_to_target_in_200_ms() {
    let now = Instant::now();
    let tween = Tween::new(A, B, now);
    assert_eq!(tween.at(now), A);
    assert!(!tween.done(now + Duration::from_millis(199)));
    assert_eq!(tween.at(now + DURATION), B);
    assert!(tween.done(now + DURATION));
    let mid = tween.at(now + DURATION / 2).w;
    assert!(mid > 200.0 && mid < 300.0, "past the linear midpoint at half time: {mid}");
}

#[test]
fn retarget_mid_flight_continues_from_the_current_frame() {
    let now = Instant::now();
    let mut tween = Tween::new(A, B, now);
    let half = now + DURATION / 2;
    let here = tween.at(half);
    tween.retarget(A, half);
    assert_eq!(tween.at(half), here, "no jump");
    assert_eq!(tween.at(half + DURATION), A);
}

/// `spring(0 → 1)` sampled every millisecond over its whole run.
fn spring_path() -> Vec<f32> {
    let total = SPRING_FOR.as_millis() as u32;
    (0..=total).map(|ms| spring(0.0, 1.0, 0.0, ms as f32 / 1000.0)).collect()
}

#[test]
fn spring_overshoots_once_by_about_a_tenth_then_lands_exactly() {
    let path = spring_path();
    let (peak_ms, peak) = path.iter().copied().enumerate().fold((0, 0.0), |m, (i, x)| if x > m.1 { (i, x) } else { m });
    assert!(peak > 1.08 && peak < 1.16, "one clear overshoot: {peak}");
    assert!((120..=200).contains(&peak_ms), "peaks early (≈160 ms): {peak_ms}");
    assert_eq!(*path.last().unwrap(), 1.0, "lands exactly");
    assert_eq!(spring(0.0, 1.0, 0.0, 10.0), 1.0);
    let after_peak = &path[peak_ms..];
    assert!(after_peak.iter().all(|x| *x > 0.97), "no second dip worth seeing");
    let steps = path.windows(2).map(|p| (p[1] - p[0]).abs()).fold(0.0, f32::max);
    assert!(steps < 0.02, "continuous at 1 ms resolution, including the tail blend: {steps}");
}

#[test]
fn spring_tween_grows_size_with_overshoot_and_fades_opacity_without_one() {
    let now = Instant::now();
    let (from, to) = (Frame { w: 300.0, h: 44.0, alpha: 0.0 }, Frame { w: 340.0, h: 58.0, alpha: 1.0 });
    let tween = Tween::spring(from, to, (0.0, 0.0), now);
    assert_eq!(tween.at(now), from);
    let frames: Vec<Frame> = (0..=42).map(|i| tween.at(now + Duration::from_millis(i * 10))).collect();
    assert!(frames.iter().any(|f| f.w > to.w + 2.0 && f.h > to.h + 1.0), "both dimensions overshoot");
    assert!(frames.iter().all(|f| f.alpha <= 1.0), "opacity never overshoots");
    assert_eq!(tween.at(now + DURATION).alpha, 1.0, "opacity is in by 200 ms");
    assert!(!tween.done(now + DURATION));
    assert_eq!(tween.at(now + SPRING_FOR), to);
    assert!(tween.done(now + SPRING_FOR));
}

#[test]
fn a_new_spring_carries_the_current_velocity_on() {
    let now = Instant::now();
    let first = Tween::spring(A, B, (0.0, 0.0), now);
    let mid = now + Duration::from_millis(60);
    let (here, velocity) = (first.at(mid), first.velocity(mid));
    assert!(velocity.0 > 500.0, "moving fast towards B: {velocity:?}");
    let second = Tween::spring(here, Frame { w: 320.0, ..B }, velocity, mid);
    assert_eq!(second.at(mid), here, "no jump");
    let carried = second.velocity(mid).0;
    assert!((carried - velocity.0).abs() / velocity.0 < 0.1, "no stall: {carried} vs {}", velocity.0);
    assert_eq!(first.velocity(now + SPRING_FOR), (0.0, 0.0), "a finished motion is at rest");
}
