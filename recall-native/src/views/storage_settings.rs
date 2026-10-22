//! Settings → Storage: SSD cache size slider, cache location, usage by kind, clear.
//! User ruling 2026-10-22: "add a cache size slider, clearly stating cache location and how
//! much to delegate (higher ssd cache means faster load responses, but takes ssd space)".
use std::{cell::Cell, path::PathBuf, rc::Rc};

use gpui::{AnyElement, Context, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, canvas, div, prelude::*, px, relative};

use crate::{
    root::{Root, ToastKind},
    services::library::format_bytes,
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
};

/// Slider stops in GB (snapping, evenly spaced on the track).
pub const CACHE_STOPS_GB: [f64; 7] = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
pub const DEFAULT_CACHE_GB: f64 = 2.0;
/// Cache sizes are binary gigabytes (the storage budget is 2 GiB = 2,147,483,648 bytes).
const GB: f64 = 1_073_741_824.0;

pub struct CacheView {
    pub root: PathBuf,
    pub budget_bytes: u64,
    pub used_bytes: u64,
    /// (label, bytes) per kind of cached data.
    pub tiers: Vec<(&'static str, u64)>,
    pub ssd_free: Option<u64>,
    pub smart: String,
    pub span: Rc<Cell<Option<(f32, f32)>>>,
    pub dragging: bool,
}

impl CacheView {
    pub fn new() -> Self {
        let root = crate::cache::cache_root();
        let ssd_free = crate::services::library::drive_space(&root).map(|(free, _)| free);
        Self {
            root,
            budget_bytes: crate::cache::budget_bytes(),
            used_bytes: 0,
            tiers: Vec::new(),
            ssd_free,
            smart: String::new(),
            span: Rc::new(Cell::new(None)),
            dragging: false,
        }
    }

    /// Re-reads usage from disk (cheap: directory sizes of the cache folders).
    pub fn refresh(&mut self) {
        let usage = crate::cache::usage();
        self.used_bytes = usage.iter().map(|(_, b)| *b).sum();
        self.tiers = usage;
        self.budget_bytes = crate::cache::budget_bytes();
        self.ssd_free = crate::services::library::drive_space(&self.root).map(|(free, _)| free);
        self.smart = crate::cache::smart_status();
    }

    fn stop_index(&self) -> usize {
        let gb = self.budget_bytes as f64 / GB;
        CACHE_STOPS_GB
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1 - gb).abs().total_cmp(&(b.1 - gb).abs()))
            .map_or(2, |(i, _)| i)
    }

    /// Snaps a window x on the slider to a stop and shows it; returns true when it moved.
    /// Nothing is applied until [`Self::commit`]: each applied size evicts down to it, so a
    /// sweep across the small stops must not empty the cache on the way.
    pub fn drag_to(&mut self, window_x: f32) -> bool {
        let Some((left, width)) = self.span.get() else { return false };
        let f = ((window_x - left) / width.max(1.0)).clamp(0.0, 1.0);
        let index = (f * (CACHE_STOPS_GB.len() - 1) as f32).round() as usize;
        let bytes = (CACHE_STOPS_GB[index] * GB) as u64;
        if bytes != self.budget_bytes {
            self.budget_bytes = bytes;
            return true;
        }
        false
    }

    /// Ends a drag: applies the shown size if it changed, then re-reads usage.
    pub fn commit(&mut self) {
        self.dragging = false;
        if self.budget_bytes != crate::cache::budget_bytes() {
            crate::cache::set_budget_bytes(self.budget_bytes);
        }
        self.refresh();
    }
}

impl Root {
    pub fn render_storage_section(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let view = &self.cache;
        let stops = CACHE_STOPS_GB.len() - 1;
        let fraction = view.stop_index() as f32 / stops as f32;
        let budget = view.budget_bytes;
        let used = view.used_bytes.min(budget.max(1));
        let span = view.span.clone();
        let root_text = view.root.to_string_lossy().into_owned();
        let open_root = view.root.clone();
        let free = view.ssd_free.map(|f| format!("{} free on this drive", format_bytes(f))).unwrap_or_default();

        let slider = div()
            .id("cache-slider")
            .h(px(28.))
            .w_full()
            .relative()
            .cursor_pointer()
            .child(canvas(move |b, _, _| span.set(Some((f32::from(b.origin.x), f32::from(b.size.width)))), |_, _, _, _| {}).absolute().inset_0())
            .child(div().absolute().left_0().right_0().top(px(12.)).h(px(4.)).rounded(px(2.)).bg(c(theme::TOP)))
            .child(div().absolute().left_0().top(px(12.)).h(px(4.)).rounded(px(2.)).w(relative(fraction)).bg(c(theme::MUTED)))
            .children((0..=stops).map(|i| {
                div()
                    .absolute()
                    .top(px(11.))
                    .left(relative(i as f32 / stops as f32))
                    .ml(px(-1.))
                    .w(px(2.))
                    .h(px(6.))
                    .bg(c(if (i as f32 / stops as f32) <= fraction { theme::MUTED } else { theme::BORDER_STRONG }))
            }))
            .child(div().absolute().top(px(7.)).left(relative(fraction)).ml(px(-7.)).size(px(14.)).rounded(px(7.)).bg(c(theme::TEXT)))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| {
                this.cache.dragging = true;
                this.cache.drag_to(f32::from(e.position.x));
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.cache.dragging {
                    if e.pressed_button != Some(MouseButton::Left) {
                        this.cache.commit();
                        cx.notify();
                    } else if this.cache.drag_to(f32::from(e.position.x)) {
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.cache.commit();
                cx.notify();
            }))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, cx| {
                if this.cache.dragging {
                    this.cache.commit();
                    cx.notify();
                }
            }));

        // Labels sit exactly under their ticks; 2 GB carries the recommendation.
        let labels = div().relative().h(px(30.)).text_size(px(11.)).text_color(c(theme::TERTIARY)).children(CACHE_STOPS_GB.iter().enumerate().map(|(i, gb)| {
            let text = if *gb < 1.0 { "0.5 GB".to_owned() } else { format!("{gb:.0} GB") };
            let recommended = (*gb - DEFAULT_CACHE_GB).abs() < f64::EPSILON;
            let label = div()
                .absolute()
                .top_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(text)
                .when(recommended, |d| d.child(div().text_size(px(10.)).font_weight(FontWeight::MEDIUM).text_color(c(theme::MUTED)).child("Recommended")));
            match i {
                0 => label.left_0().items_start(),
                n if n == stops => label.right_0().items_end(),
                _ => label.left(relative(i as f32 / stops as f32)).ml(px(-40.)).w(px(80.)).items_center(),
            }
        }));

        let usage_fraction = if budget > 0 { used as f32 / budget as f32 } else { 0.0 };
        let tiers = view
            .tiers
            .iter()
            .filter(|(_, b)| *b > 0)
            .map(|(label, b)| format!("{label} {}", format_bytes(*b)))
            .collect::<Vec<_>>()
            .join(" · ");

        let size_card = div()
            .p(px(16.))
            .rounded(px(10.))
            .bg(c(theme::RAISED))
            .border_1()
            .border_color(c(theme::BORDER_SUBTLE))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(div().font_weight(FontWeight::MEDIUM).child("SSD cache size"))
                            .child(div().text_size(px(12.)).line_height(px(17.)).text_color(c(theme::TERTIARY)).child(
                                "Larger loads previews faster but uses more SSD space.",
                            )),
                    )
                    .child(div().flex_none().font_family(theme::display()).text_size(px(18.)).font_weight(FontWeight::BOLD).child(if budget as f64 >= GB {
                        format!("{:.0} GB", budget as f64 / GB)
                    } else {
                        format!("{:.1} GB", budget as f64 / GB)
                    })),
            )
            .child(div().flex().flex_col().gap(px(6.)).child(slider).child(labels))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(12.))
                            .child(div().text_color(c(theme::MUTED)).child(if view.used_bytes == 0 {
                                "Nothing cached yet".to_owned()
                            } else {
                                format!("{} used of {}", format_bytes(view.used_bytes), gb_label(budget))
                            }))
                            .child(div().text_color(c(theme::TERTIARY)).child(free)),
                    )
                    .child(div().h(px(4.)).rounded(px(2.)).bg(c(theme::TOP)).child(div().h_full().rounded(px(2.)).w(relative(usage_fraction.clamp(0.0, 1.0))).bg(c(theme::MUTED))))
                    .when(!tiers.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(c(theme::TERTIARY)).child(tiers)))
                    .when(!view.smart.is_empty(), |d| d.child(div().flex().items_center().gap(px(6.)).text_size(px(11.)).text_color(c(theme::MUTED)).child(icon(Icon::Sun, 12., theme::MUTED)).child(view.smart.clone()))),
            );

        let location = div()
            .flex()
            .items_center()
            .gap(px(24.))
            .py(px(4.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().font_weight(FontWeight::MEDIUM).child("Cache location"))
                    .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).truncate().child(root_text)),
            )
            .child(ui::button("cache-open", Variant::Secondary).child(icon(Icon::Folder, 14., theme::TEXT)).child("Open").on_click(cx.listener(move |_, _, _, _| {
                let _ = std::fs::create_dir_all(&open_root);
                let _ = std::process::Command::new("explorer.exe").arg(&open_root).spawn();
            })))
            .child(ui::button("cache-clear", Variant::Ghost).border_1().border_color(c(theme::BORDER)).child(icon(Icon::Trash, 14., theme::MUTED)).child("Clear").on_click(cx.listener(|this, _, _, cx| {
                match crate::cache::clear() {
                    Ok(freed) => {
                        this.show_toast(ToastKind::Good, "Cache cleared", Some(format!("{} freed; previews rebuild as you browse", format_bytes(freed))));
                        this.workers.job(crate::worker::Job::List);
                    }
                    Err(error) => this.show_toast(ToastKind::Bad, "Couldn't clear the cache", Some(error)),
                }
                this.cache.refresh();
                cx.notify();
            })));

        vec![size_card.into_any_element(), location.into_any_element()]
    }
}

/// "2 GB", "0.5 GB" in the slider's binary units.
fn gb_label(bytes: u64) -> String {
    let gb = bytes as f64 / GB;
    if gb >= 1.0 && (gb - gb.round()).abs() < 0.05 { format!("{:.0} GB", gb.round()) } else { format!("{gb:.1} GB") }
}
