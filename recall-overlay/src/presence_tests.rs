use super::*;

const PILL: Frame = Frame { w: 300.0, h: 44.0, alpha: 1.0 };

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn the_pop_overshoots_a_little_and_lands_exactly() {
    assert_eq!((ease_out_back(0.0), ease_out_back(1.0)), (0.0, 1.0));
    let peak = (0..=100).map(|i| ease_out_back(i as f32 / 100.0)).fold(0.0, f32::max);
    assert!(peak > 1.04 && peak < 1.07, "a small spring: {peak}");
}

#[test]
fn a_circle_pops_in_then_unravels_into_the_pill() {
    let now = Instant::now();
    let mut presence = Presence::hidden(now);
    presence.show(now);
    assert_eq!(presence.shape(now), (0.0, 0.0), "nothing on the first instant");
    let popping: Vec<(f32, f32)> = (0..=30).map(|i| presence.shape(now + ms(i * 5))).collect();
    assert!(popping.iter().all(|&(_, u)| u == 0.0), "a circle for the whole pop");
    assert!(popping.iter().any(|&(k, _)| k > 1.03), "with an overshoot");
    assert_eq!(presence.shape(now + POP), (1.0, 0.0), "the circle is the pill's height at 150 ms");
    let unrolls: Vec<f32> = (0..=50).map(|i| presence.shape(now + POP + ms(i * 5)).1).collect();
    assert!(unrolls.windows(2).all(|p| p[1] >= p[0]), "unrolls one way");
    assert_eq!(presence.shape(now + POP + UNRAVEL), (1.0, 1.0));
    assert_eq!(presence.opened_at(), now + POP + UNRAVEL);
    assert!(!presence.moving(now + POP + UNRAVEL));
}

#[test]
fn hiding_waits_for_the_content_then_rolls_up_and_shrinks_away() {
    let now = Instant::now();
    let mut presence = Presence::hidden(now);
    presence.show(now);
    let hide = now + Duration::from_secs(3);
    presence.hide(hide, FADE_OUT);
    assert_eq!(presence.shape(hide + FADE_OUT), (1.0, 1.0), "still open while the content fades");
    let rolled = presence.shape(hide + FADE_OUT + UNRAVEL);
    assert!(rolled.1 < 1e-5 && (rolled.0 - 1.0).abs() < 1e-5, "rolled back into the circle: {rolled:?}");
    assert!(!presence.gone(hide + FADE_OUT + UNRAVEL + POP - ms(1)));
    assert!(presence.gone(hide + FADE_OUT + UNRAVEL + POP));
}

#[test]
fn a_reversal_mid_flight_carries_on_from_where_it_is() {
    let now = Instant::now();
    let mut presence = Presence::hidden(now);
    presence.show(now);
    let mid = now + ms(260);
    let here = presence.shape(mid);
    presence.hide(mid, FADE_OUT);
    assert_eq!(presence.shape(mid), here, "an unfinished pill rolls straight back, no wait");
    let back = mid + ms(80);
    let there = presence.shape(back);
    presence.show(back);
    assert_eq!(presence.shape(back), there);
    let left = presence.opened_at().saturating_duration_since(back).as_secs_f32();
    assert!((left - 0.220).abs() < 1e-3, "opens in the time left (0.55 of 400 ms): {left}");
}

#[test]
fn looks_place_the_circle_at_the_anchored_end_and_unroll_left() {
    let circle = Look::shaped(PILL, 0.5, 0.0);
    assert_eq!((circle.box_w, circle.box_h, circle.pill), (44.0, 44.0, [11.0, 11.0, 22.0, 22.0]), "centred in the end square");
    assert_eq!((circle.margin, circle.window()), (0.0, (44, 44)));
    let swell = Look::shaped(PILL, 1.06, 0.0);
    assert_eq!(swell.margin, 2.0, "the overshoot gets room around the box");
    assert_eq!(swell.window(), (48, 48));
    let half = Look::shaped(PILL, 1.0, 0.5);
    assert_eq!((half.box_w, half.pill), (172.0, [0.0, 0.0, 172.0, 44.0]), "the right end stays put");
    let open = Look::open(PILL);
    assert_eq!((open.box_w, open.pill, open.window()), (300.0, [0.0, 0.0, 300.0, 44.0], (300, 44)));
}

#[test]
fn the_mark_rides_the_left_end_and_settles_into_place() {
    // Content 260 wide centred in a 300 pill: its mark settles at 20 + 20.
    let pose = |pop, unroll| icon_pose(&Look::shaped(PILL, pop, unroll), 260.0, 20.0);
    assert_eq!(pose(0.5, 0.0), (22.0, 22.0, 0.5), "centred in the small circle, scaled with it");
    assert_eq!(pose(1.0, 0.0), (22.0, 22.0, 1.0));
    let (x, ..) = pose(1.0, 0.5);
    assert!((x - 31.0).abs() < 1e-4, "half way from the circle's centre to its place: {x}");
    assert_eq!(pose(1.0, 1.0), (40.0, 22.0, 1.0));
}
