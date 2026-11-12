//! Tests for `editor_state_edits.rs`: I/O on the playhead's part, trims of very short parts.
use super::super::{Drag, EditorState};
use crate::edit_model::{ClipInfo, TimeRange};

fn editor(ranges: Vec<TimeRange>) -> EditorState {
    let mut ed = EditorState::new(Err("no preview".into()));
    ed.info = Some(ClipInfo { path: "a.mp4".into(), duration: 60.0, width: 4, height: 2, video_fps: 60.0, source_bytes: 1, source_modified_ms: 1, audio: Vec::new(), keyframes: Vec::new(), ..Default::default() });
    ed.plan.ranges = ranges;
    ed
}

fn three_parts() -> EditorState {
    editor(vec![TimeRange::new(0.0, 10.0), TimeRange::new(20.0, 30.0), TimeRange::new(40.0, 50.0)])
}

#[test]
fn in_trims_the_part_under_the_playhead_not_the_selected_one() {
    // E3: part 3 selected, the playhead moved into part 1 by keys: I used to pull part 3's
    // start back to 30 and bring the cut 30-40 back into the export.
    let mut ed = three_parts();
    ed.selected = 2;
    ed.pending_seek = Some(5.0);
    ed.set_in();
    assert_eq!(ed.plan.ranges[0].start, 5.0);
    assert_eq!((ed.plan.ranges[2].start, ed.plan.ranges[2].end), (40.0, 50.0), "the selected part stays");
    assert_eq!(ed.selected, 0, "the trimmed part is selected");
}

#[test]
fn out_trims_the_part_under_the_playhead_not_the_selected_one() {
    let mut ed = three_parts();
    ed.selected = 0;
    ed.pending_seek = Some(45.0);
    ed.set_out();
    assert_eq!(ed.plan.ranges[2].end, 45.0);
    assert_eq!((ed.plan.ranges[0].start, ed.plan.ranges[0].end), (0.0, 10.0), "the cut 10-20 stays cut");
    assert_eq!(ed.selected, 2);
}

#[test]
fn in_and_out_in_a_cut_move_the_part_next_to_the_playhead() {
    let mut ed = three_parts();
    ed.selected = 2;
    ed.pending_seek = Some(15.0);
    ed.set_in();
    assert_eq!(ed.plan.ranges[1].start, 15.0, "I in a cut: the part after it starts here");
    assert_eq!(ed.plan.ranges[2].start, 40.0);
    let mut ed = three_parts();
    ed.selected = 0;
    ed.pending_seek = Some(35.0);
    ed.set_out();
    assert_eq!(ed.plan.ranges[1].end, 35.0, "O in a cut: the part before it ends here");
    assert_eq!(ed.plan.ranges[0].end, 10.0);
}

#[test]
fn trimming_a_part_shorter_than_the_minimum_does_not_panic() {
    // E10: a loaded plan may hold a 0.02 s part; f64::clamp(min > max) panicked on the UI thread.
    for drag in [Drag::TrimStart(1), Drag::TrimEnd(1)] {
        let mut ed = editor(vec![TimeRange::new(0.0, 1.0), TimeRange::new(1.0, 1.02), TimeRange::new(1.02, 60.0)]);
        ed.track_span.set(Some((0.0, 600.0)));
        ed.begin_drag(drag);
        ed.drag_to(10.0);
        ed.drag_to(590.0);
        ed.end_drag();
        let ranges = &ed.plan.ranges;
        assert!(ranges.windows(2).all(|w| w[0].end <= w[1].start), "{drag:?}: parts still in order {ranges:?}");
        assert!(ranges.iter().all(|r| r.end > r.start), "{drag:?}: every part still valid {ranges:?}");
    }
}

#[test]
fn in_or_out_at_a_shared_edge_makes_no_empty_undo_step() {
    // R2-6: I right after a split (the playhead on the shared edge) changed nothing but still
    // pushed an undo step, cleared redo and set off an autosave and a flush.
    for out in [false, true] {
        let mut ed = editor(vec![TimeRange::new(0.0, 5.0), TimeRange::new(5.0, 60.0)]);
        let kept = ed.plan.clone();
        ed.redo.push(kept.clone());
        ed.needs_flush = false;
        ed.pending_seek = Some(5.0);
        if out { ed.set_out() } else { ed.set_in() }
        assert_eq!(ed.plan.ranges, kept.ranges, "out={out}");
        assert!(ed.undo.is_empty(), "out={out}: no empty undo step");
        assert_eq!(ed.redo.len(), 1, "out={out}: redo is kept");
        assert!(!ed.needs_flush, "out={out}: nothing to save");
    }
}
