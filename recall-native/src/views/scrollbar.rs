//! A scroll bar on the right edge of a scrolling area, for people who would rather drag than
//! wheel. The thumb follows the wheel; dragging it scrolls; a press on the track jumps the thumb
//! there and keeps dragging. Hidden when everything fits. All sizes are logical pixels, so the
//! bar is the same size at every display scale.
//!
//! Two sources: a gpui `list` (the clip grid, [`ListScroll`]) and a plain `overflow_y_scroll`
//! div tracked by a [`ScrollHandle`] ([`area`]).
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use gpui::{
    Bounds, CursorStyle, DispatchPhase, HitboxBehavior, IntoElement, ListOffset, ListState, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ScrollHandle, canvas, div, fill, point, prelude::*, px, size,
};

use crate::theme::{self, ca};

/// Width the bar takes for the pointer (the drawn thumb is narrower).
pub const HIT_WIDTH: f32 = 12.0;
const THUMB_W: f32 = 6.0;
const THUMB_W_ACTIVE: f32 = 8.0;
/// Gap between the thumb and the area's right edge.
const RIGHT_GAP: f32 = 3.0;
/// Track inset at the top and bottom, so the thumb clears rounded panel corners.
const INSET: f32 = 4.0;
const MIN_THUMB: f32 = 32.0;

/// What a bar reads and moves.
pub trait Scroll: Clone + 'static {
    /// (distance scrolled from the top, full content height) for a viewport `view` tall.
    fn position(&self, view: f32) -> (f32, f32);
    /// Scrolls so that `y` pixels of content sit above the viewport.
    fn scroll_to(&self, y: f32);
}

/// Thumb (top within the track, length), or None when the content fits.
pub fn thumb(track: f32, view: f32, content: f32, offset: f32) -> Option<(f32, f32)> {
    if track <= 0.0 || content <= view + 0.5 {
        return None;
    }
    let len = (track * view / content).clamp(MIN_THUMB.min(track), track);
    let travel = track - len;
    let max = content - view;
    Some((travel * (offset / max).clamp(0.0, 1.0), len))
}

/// The scroll offset that puts the thumb's top at `top` within the track.
pub fn offset_for(track: f32, view: f32, content: f32, top: f32) -> f32 {
    let Some((_, len)) = thumb(track, view, content, 0.0) else { return 0.0 };
    let travel = track - len;
    if travel <= 0.0 {
        return 0.0;
    }
    (top.clamp(0.0, travel) / travel) * (content - view)
}

thread_local! {
    /// The bar being dragged and where on its thumb the press landed. One pointer, one drag.
    static DRAG: Cell<Option<(&'static str, f32)>> = const { Cell::new(None) };
    static HANDLES: RefCell<HashMap<&'static str, ScrollHandle>> = RefCell::new(HashMap::new());
}

fn dragging(id: &'static str) -> Option<f32> {
    DRAG.with(|d| d.get()).filter(|(on, _)| *on == id).map(|(_, grab)| grab)
}

/// The scroll handle kept for `id` across frames (a new one each frame would reset to the top).
pub fn handle(id: &'static str) -> ScrollHandle {
    HANDLES.with(|h| h.borrow_mut().entry(id).or_default().clone())
}

impl Scroll for ScrollHandle {
    fn position(&self, view: f32) -> (f32, f32) {
        (-f32::from(self.offset().y), f32::from(self.max_offset().height) + view)
    }

    fn scroll_to(&self, y: f32) {
        let x = self.offset().x;
        self.set_offset(point(x, px(-y.max(0.0))));
    }
}

/// A gpui `list` whose row heights the caller knows: `starts[i]` is row i's top, and the last
/// entry is the full content height. Rows the list has not laid out yet still count, so the
/// thumb does not jump as new rows come into view.
#[derive(Clone)]
pub struct ListScroll {
    pub list: ListState,
    pub starts: Rc<Vec<f32>>,
}

impl ListScroll {
    fn total(&self) -> f32 {
        self.starts.last().copied().unwrap_or(0.0)
    }
}

impl Scroll for ListScroll {
    fn position(&self, _view: f32) -> (f32, f32) {
        let top = self.list.logical_scroll_top();
        let total = self.total();
        let base = self.starts.get(top.item_ix).copied().unwrap_or(total);
        (base + f32::from(top.offset_in_item), total)
    }

    fn scroll_to(&self, y: f32) {
        let rows = self.starts.len().saturating_sub(1);
        if rows == 0 {
            return;
        }
        let y = y.clamp(0.0, self.total());
        let ix = self.starts[..rows].partition_point(|s| *s <= y).saturating_sub(1);
        self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(y - self.starts[ix]) });
    }
}

/// Row tops from row heights, plus the total at the end.
pub fn starts(heights: impl IntoIterator<Item = f32>) -> Vec<f32> {
    let mut out = vec![0.0];
    let mut y = 0.0;
    for h in heights {
        y += h;
        out.push(y);
    }
    out
}

/// A scrolling column with a bar: a flex child that fills what its parent gives it.
pub fn area(id: &'static str, content: impl IntoElement) -> gpui::Div {
    let scroll = handle(id);
    div()
        .flex_1()
        .min_h_0()
        .relative()
        .flex()
        .flex_col()
        .child(div().id(id).flex_1().min_h_0().overflow_y_scroll().track_scroll(&scroll).child(content))
        .child(bar(id, scroll))
}

/// The bar itself: an absolute strip on the right of the nearest relative parent, which should
/// be the scrolling area's own box.
pub fn bar<S: Scroll>(id: &'static str, scroll: S) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            let view = f32::from(bounds.size.height);
            let (offset, content) = scroll.position(view);
            let track_top = f32::from(bounds.origin.y) + INSET;
            let track = view - INSET * 2.0;
            let Some((top, len)) = thumb(track, view, content, offset) else {
                if dragging(id).is_some() {
                    DRAG.with(|d| d.set(None));
                }
                return;
            };
            let grabbed = dragging(id);
            let hovered = hitbox.is_hovered(window);
            window.set_cursor_style(CursorStyle::Arrow, &hitbox);
            let active = grabbed.is_some() || hovered;
            let w = if active { THUMB_W_ACTIVE } else { THUMB_W };
            let alpha = if grabbed.is_some() { 0.46 } else if hovered { 0.32 } else { 0.2 };
            let right = f32::from(bounds.origin.x + bounds.size.width) - RIGHT_GAP;
            if active {
                let lane = Bounds { origin: point(px(right - w), px(track_top)), size: size(px(w), px(track)) };
                window.paint_quad(fill(lane, ca(theme::TEXT, 0.06)).corner_radii(px(w / 2.0)));
            }
            let knob = Bounds { origin: point(px(right - w), px(track_top + top)), size: size(px(w), px(len)) };
            window.paint_quad(fill(knob, ca(theme::TEXT, alpha)).corner_radii(px(w / 2.0)));

            let pressed = hitbox.clone();
            let press_scroll = scroll.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !pressed.is_hovered(window) {
                    return;
                }
                let y = f32::from(event.position.y) - track_top;
                let on_thumb = (top..=top + len).contains(&y);
                let grab = if on_thumb { y - top } else { len / 2.0 };
                DRAG.with(|d| d.set(Some((id, grab))));
                if !on_thumb {
                    press_scroll.scroll_to(offset_for(track, view, content, y - grab));
                }
                cx.stop_propagation();
                window.refresh();
            });
            // No hitbox on move and release: the drag keeps following the pointer off the bar,
            // over the title bar and outside the window.
            let hover_box = hitbox.clone();
            let move_scroll = scroll.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                if let Some(grab) = dragging(id) {
                    if event.pressed_button == Some(MouseButton::Left) {
                        let y = f32::from(event.position.y) - track_top - grab;
                        move_scroll.scroll_to(offset_for(track, view, content, y));
                    } else {
                        DRAG.with(|d| d.set(None));
                    }
                    cx.stop_propagation();
                    window.refresh();
                } else if hover_box.is_hovered(window) != hovered {
                    window.refresh();
                }
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, _| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left && dragging(id).is_some() {
                    DRAG.with(|d| d.set(None));
                    window.refresh();
                }
            });
        },
    )
    .absolute()
    .top_0()
    .bottom_0()
    .right_0()
    .w(px(HIT_WIDTH))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_thumb_when_content_fits() {
        assert_eq!(thumb(492.0, 500.0, 500.0, 0.0), None);
        assert_eq!(thumb(492.0, 500.0, 300.0, 0.0), None);
    }

    #[test]
    fn thumb_spans_the_visible_share() {
        let (top, len) = thumb(400.0, 400.0, 1600.0, 0.0).unwrap();
        assert_eq!((top, len), (0.0, 100.0));
        let (top, _) = thumb(400.0, 400.0, 1600.0, 1200.0).unwrap();
        assert_eq!(top, 300.0);
        let (top, _) = thumb(400.0, 400.0, 1600.0, 600.0).unwrap();
        assert_eq!(top, 150.0);
    }

    #[test]
    fn long_content_keeps_a_grabbable_thumb() {
        let (_, len) = thumb(400.0, 400.0, 1_000_000.0, 0.0).unwrap();
        assert_eq!(len, MIN_THUMB);
    }

    #[test]
    fn drag_position_round_trips() {
        for offset in [0.0, 37.0, 600.0, 1200.0] {
            let (top, _) = thumb(400.0, 400.0, 1600.0, offset).unwrap();
            assert!((offset_for(400.0, 400.0, 1600.0, top) - offset).abs() < 0.01);
        }
        assert_eq!(offset_for(400.0, 400.0, 1600.0, -50.0), 0.0);
        assert_eq!(offset_for(400.0, 400.0, 1600.0, 10_000.0), 1200.0);
    }

    #[test]
    fn starts_add_up_row_heights() {
        assert_eq!(starts([32.0, 200.0, 48.0]), vec![0.0, 32.0, 232.0, 280.0]);
    }
}
