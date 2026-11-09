//! Gate A1 (adopted game folders): a clip the saver writes in place, or the mover lands after a
//! staged copy, joins the folder's `saved` list, so the folder owns it. Temp folders only.
use super::*;
use crate::game_folder_rule as rule;

fn owned(dir: &Path, name: &str) -> bool {
    rule::read_marker(dir).is_some_and(|m| m.owns(name))
}

#[test]
fn a_clip_saved_in_place_into_an_adopted_folder_is_recalls() {
    let out = temp_dir("adopted-inplace");
    std::fs::write(out.join("VALORANT__2025-01-01__10-00-00.mp4"), b"the user's").unwrap();
    rule::adopt(&out).unwrap();
    let (events, sink) = Events::capture(true);
    let (queue, control) = spawn_staged(events, fixed_mux(10), no_gallery(), None);
    assert!(queue.submit(job(&out.join(CLIP))));
    wait_for(&sink, "saved", 1);
    assert!(owned(&out, CLIP), "{:?}", rule::read_marker(&out));
    assert!(!owned(&out, "VALORANT__2025-01-01__10-00-00.mp4"));
    drop(queue);
    assert_eq!(control.shutdown(Duration::from_secs(3)), Shutdown::Drained);
}

#[test]
fn a_clip_the_mover_lands_in_an_adopted_folder_is_recalls() {
    let (stage, out) = (temp_dir("adopted-stage"), temp_dir("adopted-out"));
    rule::adopt(&out).unwrap();
    let (events, sink) = Events::capture(true);
    let cfg = setup(Some(&stage), &stage, &out, |_, _| false);
    let (queue, control) = spawn_staged(events, fixed_mux(5_000), no_gallery(), Some(cfg));
    assert!(queue.submit(job(&out.join(CLIP))));
    wait_for(&sink, "clip_moved", 1);
    assert!(owned(&out, CLIP), "{:?}", rule::read_marker(&out));
    assert!(!owned(&stage, CLIP), "the staging folder has no marker");
    drop(queue);
    assert_eq!(control.shutdown(Duration::from_secs(3)), Shutdown::Drained);
}
