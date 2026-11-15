use super::*;

const SEC: Duration = Duration::from_secs(1);

#[test]
fn only_a_frame_on_the_cpu_path_redraws() {
    assert!(redraws(Wake::Frame, true));
    assert!(!redraws(Wake::Frame, false), "the GPU path has no redraw loop");
    for why in [Wake::Quit, Wake::Screenshot] {
        assert!(!redraws(why, true) && !redraws(why, false), "{why:?}");
    }
}

#[test]
fn the_first_periodic_is_due_at_once() {
    let now = Instant::now();
    assert!(periodic_due(None, SEC, now));
    assert_eq!(next_deadline(None, SEC, now), now);
}

#[test]
fn the_loop_sleeps_until_the_next_second() {
    let t0 = Instant::now();
    let now = t0 + Duration::from_millis(300);
    assert!(!periodic_due(Some(t0), SEC, now));
    assert_eq!(next_deadline(Some(t0), SEC, now), t0 + SEC);
    assert!(periodic_due(Some(t0), SEC, t0 + SEC));
}

#[test]
fn a_missed_deadline_is_never_in_the_past() {
    let t0 = Instant::now();
    let late = t0 + Duration::from_secs(3);
    assert_eq!(next_deadline(Some(t0), SEC, late), late);
    assert!(periodic_due(Some(t0), SEC, late));
}

#[test]
fn frame_wakes_coalesce_until_taken() {
    let pending = AtomicBool::new(false);
    assert!(claim(&pending), "the first frame sends");
    assert!(!claim(&pending) && !claim(&pending), "later frames ride the pending wake");
    pending.store(false, Release); // the loop took it
    assert!(claim(&pending), "the next frame sends again");
}

#[test]
fn wakes_without_a_loop_are_no_ops() {
    // No test installs the proxy: nothing is sent and the frame flag is released.
    assert!(!wake(Wake::Quit));
    frame();
    assert!(!FRAME_PENDING.load(std::sync::atomic::Ordering::Acquire));
}
