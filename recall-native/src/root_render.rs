//! Drawing the top-level view: frame scheduling, the backdrop around the native video, the body.
use gpui::{Bounds, Context, Render, Window, canvas, div, fill, point, prelude::*, px, size};

use super::{Root, Route};
use crate::theme::{self, c};

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_render_marked {
            self.first_render_marked = true;
            crate::startup::mark("first render");
        }
        let body = if crate::TESTING {
            self.settle_fps_input(window);
            if self.setup.is_some() {
                self.render_setup(window, cx)
            } else {
                self.render_testing_library(cx)
            }
        } else {
            self.settle_focus(window);
            self.trace_ui(window);
            crate::trace::clear_targets(window);
            self.schedule_frames(window, cx);
            match self.route {
                Route::Editor => self.render_editor(window, cx),
                _ => self.render_shell(window, cx),
            }
        };
        let backdrop = self.backdrop();
        let mut root = div();
        // Tabular figures everywhere so timecodes, sizes and counters don't jitter.
        root.text_style().get_or_insert_with(Default::default).font_features =
            Some(gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])));
        // Focus tracking stays off the title bar: a focus-tracked element marks every mouse-down
        // under it as handled, and GPUI's Windows caption code then skips minimize / maximize /
        // close, title-bar drag and double-click maximize.
        let body = div()
            .id("body")
            .key_context("Recall")
            .track_focus(&self.focus)
            .capture_any_mouse_down(cx.listener(Self::on_press))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if crate::TESTING {
                    if this.audio_drag_key(event, window, cx)
                        || this.audio_name_key(event, None, window)
                        || this.fps_input_key(event, None, window)
                    {
                        cx.notify();
                    } else if event.keystroke.key == "escape" {
                        this.settings_ui.audio.menu = None;
                        this.settings_ui.quality_preview = None;
                        cx.notify();
                    }
                } else {
                    this.on_key(event, window, cx);
                }
            }))
            .flex_1()
            .min_h_0()
            .flex()
            .child(body);
        root.id("root")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(backdrop)
            .child(Self::drag_watch(cx))
            .font_family(theme::sans())
            // Rohan 11-10: one step heavier everywhere (OLED subpixel layouts thin 400 Geist).
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_size(px(13.))
            .line_height(px(18.))
            .text_color(c(theme::TEXT))
            .child(self.render_titlebar(window, cx))
            .child(body)
            .children(self.render_toast(cx))
            .children(self.render_update(cx))
            .children(self.render_game_folder_prompt(cx))
            .children(self.render_recovered_prompt(cx))
    }
}

impl Root {
    /// Animation frames while meters or playback move, and a preview re-place right after a
    /// frame lays it out at a new size.
    fn schedule_frames(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Meters and playback animate at the display refresh rate, not the 30 ms poll.
        if self.route == Route::Audio || (self.route == Route::Editor && (self.ed.playing || (!self.ed.loading.grey.is_empty() && self.ed.loading.failed.is_none()))) {
            window.request_animation_frame();
        }
        // Right after a frame lays out the preview at a new size (resize, maximize), move the
        // native video there and repaint, so the cut-out never waits for the 30 ms poll.
        if self.route == Route::Editor {
            let root = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = root.update(cx, |this, cx| {
                    if this.ed.apply_preview_rect(true) {
                        cx.notify();
                    }
                });
            });
        }
    }

    /// The window colour, painted around the native video window so it shows through.
    fn backdrop(&self) -> impl IntoElement + use<> {
        let hole = self.ed.hole.clone();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let color = c(theme::WINDOW);
                let Some((x, y, w, h)) = hole.get() else {
                    window.paint_quad(fill(bounds, color));
                    return;
                };
                // Paint around the native video window so it shows through.
                let s = window.scale_factor();
                let (hx, hy, hw, hh) = (x as f32 / s, y as f32 / s, w as f32 / s, h as f32 / s);
                let (bx, by) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let rect = |x0: f32, y0: f32, x1: f32, y1: f32| {
                    Bounds::new(point(px(x0), px(y0)), size(px((x1 - x0).max(0.0)), px((y1 - y0).max(0.0))))
                };
                window.paint_quad(fill(rect(bx, by, bx + bw, hy), color));
                window.paint_quad(fill(rect(bx, hy + hh, bx + bw, by + bh), color));
                window.paint_quad(fill(rect(bx, hy, hx, hy + hh), color));
                window.paint_quad(fill(rect(hx + hw, hy, bx + bw, hy + hh), color));
            },
        )
        .absolute()
        .inset_0()
    }
}
