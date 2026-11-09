//! Library grid: the virtualized day-grouped list, thumbnail tiles (hover scrub,
//! drag-out, rename, right-click menu in `tile_menu.rs`) and their hover actions. Clicks select
//! (`library_select.rs`) or open (`tile_click.rs`); a double-click on the title renames.
use std::path::PathBuf;

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit,
    ScrollWheelEvent, Window, canvas, div, img, list, prelude::*, px,
};

use super::{CONTENT_PAD, GAP, empty_state};
use crate::{
    library_state::Row,
    root::Root,
    services::library,
    theme::{self, c, ca},
    ui::{Icon, icon},
};

impl Root {
    pub(super) fn render_library_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let width_cell = self.lib.content_width.clone();
        let measure = canvas(
            move |bounds, _, _| width_cell.set(f32::from(bounds.size.width) - CONTENT_PAD * 2.0),
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .w_full()
        .h(px(1.));
        let body = div().flex_1().min_h_0().relative().child(measure);
        if self.lib.loading {
            return body.child(empty_state(Icon::Film, "Loading your clips…", None)).into_any_element();
        }
        if let Some(error) = self.lib.error.clone() {
            return body.child(empty_state(Icon::Alert, "Couldn't read the clip folder", Some(error))).into_any_element();
        }
        if self.lib.records.is_empty() {
            let hotkey = self.snap.session.as_ref().map(|s| s.save_hotkey.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "Alt+N".into());
            return body
                .child(empty_state(Icon::Film, "No clips yet", Some(format!("Play a game and press {hotkey} to save the last two minutes."))))
                .into_any_element();
        }
        if self.lib.rows.is_empty() {
            return body.child(empty_state(Icon::Search, "No clips match", Some("Try another name, or clear the filter.".into()))).into_any_element();
        }
        body.child(
            list(self.lib.list.clone(), cx.processor(|this, ix: usize, window, cx| this.render_row(ix, window, cx)))
                .size_full(),
        )
        .on_scroll_wheel(cx.listener(|this, _: &ScrollWheelEvent, _, cx| {
            if this.close_tile_menu() {
                cx.notify();
            }
        }))
        .children(self.render_tile_menu(cx))
        .into_any_element()
    }

    fn tile_size(&self) -> (f32, f32) {
        let width = self.lib.content_width.get().max(400.0);
        let columns = self.lib.columns.max(1) as f32;
        let w = ((width - GAP * (columns - 1.0)) / columns).floor();
        (w, (w * 9.0 / 16.0).round())
    }

    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.lib.rows.get(ix).cloned() else { return div().into_any_element() };
        // Thumbnails are cached as wide as the widest tile any monitor can show (maximized, at
        // its own scale), so the size is the same whichever monitor or window size is in use.
        let chrome = f32::from(window.viewport_size().width) - self.lib.content_width.get();
        let here = (self.tile_size().0 * window.scale_factor()).ceil() as u32;
        crate::services::storage::set_thumb_width(widest_tile_cached(chrome).max(here));
        match row {
            Row::Header { group, first } => {
                let Some(g) = self.lib.groups.get(group) else { return div().into_any_element() };
                let count = g.items.len();
                let mut meta = format!("{count} {}", if count == 1 { "clip" } else { "clips" });
                if g.durations_complete && g.total_secs > 0.0 {
                    meta.push_str(&format!(" · {}", library::format_duration(g.total_secs)));
                }
                div()
                    .px(px(CONTENT_PAD))
                    .pt(px(if first { 0. } else { 16. }))
                    .pb(px(12.))
                    .flex()
                    .items_baseline()
                    .gap(px(10.))
                    .child(div().font_family(theme::display()).text_size(px(16.)).line_height(px(20.)).font_weight(FontWeight::BOLD).child(g.label.clone()))
                    .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(meta))
                    .into_any_element()
            }
            Row::Tiles(items) => {
                let columns = self.lib.columns.max(1);
                let mut children: Vec<AnyElement> = items.iter().map(|i| self.render_tile(*i, window, cx)).collect();
                while children.len() < columns {
                    children.push(div().flex_1().into_any_element());
                }
                div().px(px(CONTENT_PAD)).pb(px(20.)).flex().gap(px(GAP)).children(children).into_any_element()
            }
        }
    }

    fn render_tile(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (w, h) = self.tile_size();
        let record = self.lib.records[index].clone();
        let path = record.path.clone();
        let hovered = self.lib.hovered.as_deref() == Some(path.as_str());
        let selected = self.lib.selection.contains(&path);
        let last_played = self.lib.last_played().is_some_and(|r| r.path == path);
        let renaming = self.lib.renaming.as_deref() == Some(path.as_str());
        let image_path = self.lib.scrub_frame(&path).map(str::to_owned).or_else(|| record.thumb.clone());
        let image: AnyElement = match image_path {
            Some(p) => img(PathBuf::from(p)).w(px(w)).h(px(h)).rounded(px(8.)).object_fit(ObjectFit::Cover).into_any_element(),
            None => div()
                .w(px(w))
                .h(px(h))
                .rounded(px(8.))
                .bg(c(theme::RAISED))
                .flex()
                .flex_col()
                .gap(px(4.))
                .items_center()
                .justify_center()
                .child(icon(Icon::Film, 20., theme::DISABLED))
                .when(self.lib.thumb_waiting(&path), |d| d.text_size(px(11.)).text_color(c(theme::TERTIARY)).child("Loading…"))
                .into_any_element(),
        };
        let badge = match (hovered, record.duration) {
            (true, Some(d)) => Some(format!("{} / {}", library::format_duration(d * self.lib.hover_frac as f64), library::format_duration(d))),
            (_, Some(d)) => Some(library::format_duration(d)),
            // Read and failed (a broken file): a dash, not a wait that never ends.
            _ if self.lib.duration_unknown(&path) => Some("—".to_owned()),
            _ => Some("…".to_owned()),
        };
        let scrub_waiting = self.lib.scrub_waiting(&path);
        let span = self.lib.hover_span.clone();
        let ring = if selected { theme::TEXT } else if hovered { theme::BORDER_STRONG } else { theme::BORDER_SUBTLE };
        let mut meta = library::format_bytes(record.bytes);
        if record.edited {
            meta.push_str(" · Edited");
        }
        let time = format!("{:02}:{:02}", record.started.hour, record.started.minute);
        let media = div()
            .relative()
            .w(px(w))
            .h(px(h))
            .rounded(px(8.))
            .border(px(if selected { 2. } else { 1. }))
            .border_color(c(ring))
            .child(div().absolute().inset_0().child(image))
            .when(hovered, |d| {
                d.child(
                    canvas(move |bounds, _, _| span.set(Some((f32::from(bounds.origin.x), f32::from(bounds.size.width)))), |_, _, _, _| {})
                        .absolute()
                        .inset_0(),
                )
                .child(
                    div().absolute().left(px(8.)).right(px(8.)).bottom(px(4.)).h(px(3.)).rounded(px(2.)).bg(ca(theme::TEXT, 0.18)).child(
                        div().h_full().rounded(px(2.)).w(gpui::relative(self.lib.hover_frac)).bg(c(theme::SIGNAL)),
                    ),
                )
                .child(self.render_tile_actions(&path, record.favourite, cx))
            })
            .when(scrub_waiting, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(8.))
                        .bottom(px(10.))
                        .px(px(5.))
                        .rounded(px(4.))
                        .bg(ca(theme::WINDOW, 0.8))
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .child("Loading preview…"),
                )
            })
            .when(record.favourite && !hovered, |d| {
                d.child(div().absolute().top(px(8.)).left(px(8.)).child(icon(Icon::StarFill, 14., theme::TEXT)))
            })
            .when(last_played && !hovered && !selected, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(6.))
                        .left(px(if record.favourite { 28. } else { 8. }))
                        .px(px(5.))
                        .rounded(px(4.))
                        .bg(ca(theme::WINDOW, 0.8))
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .child("Last played"),
                )
            })
            .when(selected, |d| {
                d.child(
                    div()
                        .absolute()
                        .top(px(8.))
                        .left(px(8.))
                        .size(px(20.))
                        .rounded(px(5.))
                        .bg(c(theme::TEXT))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(Icon::Check, 14., theme::BASE)),
                )
            })
            .children(badge.map(|b| {
                div()
                    .absolute()
                    .right(px(8.))
                    .bottom(px(10.))
                    .px(px(5.))
                    .rounded(px(4.))
                    .bg(ca(theme::WINDOW, 0.8))
                    .font_family(theme::mono())
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .child(b)
            }));
        let title_row: AnyElement = if renaming {
            let painted = self.rename_painted.clone();
            div()
                .id("rename-row")
                .relative()
                .h(px(26.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(5.))
                .bg(c(theme::RAISED))
                .border_1()
                .border_color(c(theme::BORDER_STRONG))
                .child(
                    // The list also lays out rows just past the viewport: those don't count.
                    canvas(move |bounds, window, _| painted.set(!bounds.intersect(&window.content_mask().bounds).is_empty()), |_, _, _, _| {})
                        .absolute()
                        .inset_0(),
                )
                .child(crate::trace::target("rename"))
                .child(self.rename.render("rename", window).flex_1())
                // Inside the field a press is for the field: no opening, dragging or menu.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.rename.deselect();
                    cx.stop_propagation();
                    cx.notify();
                }))
                .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                    this.end_rename(crate::library_state::RenameEnd::Away, window);
                    cx.notify();
                }))
                .into_any_element()
        } else {
            let title_path = path.clone();
            let title_target = format!("title:{}", std::path::Path::new(&path).file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()));
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .child(crate::trace::target(title_target))
                        .id("tile-title")
                        .font_weight(FontWeight::MEDIUM)
                        .min_w_0()
                        .truncate()
                        .child(record.display_title().to_owned())
                        // Waits out a double-click before opening, so a double-click renames.
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.tile_title_click(&title_path, event, window, cx);
                            cx.notify();
                        })),
                )
                .child(div().font_family(theme::mono()).text_size(px(11.)).text_color(c(theme::TERTIARY)).child(time))
                .into_any_element()
        };
        let open_path = path.clone();
        let hover_path = path.clone();
        let drag_path = path.clone();
        let menu_path = path.clone();
        let target = format!("tile:{}", std::path::Path::new(&path).file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()));
        div()
            .child(crate::trace::target(target))
            .id(("tile", index))
            .w(px(w))
            .flex()
            .flex_col()
            .gap(px(8.))
            .cursor_pointer()
            .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                if *over {
                    this.lib.set_hover(Some(hover_path.clone()), &this.workers);
                } else if this.lib.hovered.as_deref() == Some(hover_path.as_str()) {
                    this.lib.set_hover(None, &this.workers);
                }
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, _| {
                this.drag_origin = Some((drag_path.clone(), f32::from(event.position.x), f32::from(event.position.y)));
            }))
            // Windows opens context menus on button release.
            .on_mouse_up(MouseButton::Right, cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                this.open_tile_menu(menu_path.clone(), event.position);
                cx.stop_propagation();
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    if let Some((path, x0, y0)) = this.drag_origin.clone() {
                        let (x, y) = (f32::from(event.position.x), f32::from(event.position.y));
                        if (x - x0).abs() + (y - y0).abs() > 8.0 {
                            this.drag_origin = None;
                            this.start_drag(path, cx);
                        }
                    }
                    return;
                }
                if let Some((left, width)) = this.lib.hover_span.get() {
                    let frac = ((f32::from(event.position.x) - left) / width.max(1.0)).clamp(0.0, 0.999);
                    if (frac - this.lib.hover_frac).abs() > 0.004 {
                        this.lib.hover_frac = frac;
                        cx.notify();
                    }
                }
            }))
            // A plain click opens at once; Shift and Ctrl select (`tile_click.rs`).
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.tile_body_click(&open_path, event);
                cx.notify();
            }))
            .child(media)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(title_row)
                    .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::TERTIARY)).child(meta)),
            )
            .into_any_element()
    }

    fn render_tile_actions(&self, path: &str, favourite: bool, cx: &mut Context<Self>) -> AnyElement {
        let open_path = path.to_owned();
        let fav_path = path.to_owned();
        let folder_path = path.to_owned();
        let rename_path = path.to_owned();
        let copy_path = path.to_owned();
        div()
            .absolute()
            .top(px(6.))
            .right(px(6.))
            .flex()
            .gap(px(4.))
            .child(tile_action("tile-open", Icon::Play).on_click(cx.listener(move |this, _, _, cx| {
                this.open_clip(open_path.clone());
                this.opened_by_click = true;
                cx.stop_propagation();
                cx.notify();
            })))
            .child(tile_action("tile-fav", if favourite { Icon::StarFill } else { Icon::Star }).on_click(cx.listener(move |this, _, _, cx| {
                this.lib.toggle_favourite(std::slice::from_ref(&fav_path));
                cx.stop_propagation();
                cx.notify();
            })))
            .child(tile_action("tile-rename", Icon::Pencil).on_click(cx.listener(move |this, _, window, cx| {
                this.begin_rename(rename_path.clone(), window);
                cx.stop_propagation();
                cx.notify();
            })))
            .child(tile_action("tile-copy", Icon::Copy).on_click(cx.listener(move |this, _, _, cx| {
                this.copy_clips(vec![copy_path.clone()]);
                cx.stop_propagation();
                cx.notify();
            })))
            .child(tile_action("tile-folder", Icon::Folder).on_click(cx.listener(move |_, _, _, cx| {
                crate::backend::show_in_folder(&folder_path);
                cx.stop_propagation();
            })))
            .into_any_element()
    }
}

fn tile_action(id: &'static str, kind: Icon) -> gpui::Stateful<gpui::Div> {
    div()
        .child(crate::trace::target(id))
        .id(id)
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(ca(theme::BASE, 0.86))
        .border_1()
        .border_color(c(theme::BORDER))
        .hover(|s| s.bg(c(theme::FLOATING)))
        .child(icon(kind, 14., theme::TEXT))
}

/// [`crate::display_tiles::widest_tile`], re-measured at most every 2 s (a monitor plugged in,
/// a scale change) or when the window's chrome width changes.
fn widest_tile_cached(chrome: f32) -> u32 {
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    thread_local! {
        static LAST: Cell<Option<(Instant, f32, u32)>> = const { Cell::new(None) };
    }
    LAST.with(|last| match last.get() {
        Some((at, seen, widest)) if at.elapsed() < Duration::from_secs(2) && (seen - chrome).abs() < 1.0 => widest,
        _ => {
            let widest = crate::display_tiles::widest_tile(chrome);
            last.set(Some((Instant::now(), chrome, widest)));
            crate::startup::mark(&format!("thumbnail width {widest} px (chrome {chrome:.0} px)"));
            widest
        }
    })
}
