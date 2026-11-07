//! Settings → Storage: SSD cache size slider, cache location, usage by kind, clear.
//! User ruling 2026-10-22: "add a cache size slider, clearly stating cache location and how
//! much to delegate (higher ssd cache means faster load responses, but takes ssd space)".
use std::{cell::Cell, path::PathBuf, rc::Rc};

use gpui::{AnyElement, Context, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, Window, canvas, div, prelude::*, px, relative};

use crate::{
    root::{Root, ToastKind},
    services::library::format_bytes,
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
};

/// Slider stops in GB (snapping, evenly spaced on the track).
/// 2 is the first-run budget on the system drive, 10 and 20 common ones on another SSD
/// (`evict::default_budget`); 50 is for a large SSD.
pub const CACHE_STOPS_GB: [f64; 6] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0];
/// The recommended stop: the system drive's first-run budget (Rohan 2026-11-03: 2 GB on C:).
pub const DEFAULT_CACHE_GB: f64 = 2.0;
/// Cache sizes are binary gigabytes (20 GiB = 21,474,836,480 bytes).
const GB: f64 = 1_073_741_824.0;

pub struct CacheView {
    pub root: PathBuf,
    pub budget_bytes: u64,
    /// Set while storage.json can't be read: the size is read-only (a guess must not be saved).
    pub budget_note: Option<String>,
    pub used_bytes: u64,
    /// (label, bytes) per kind of cached data.
    pub tiers: Vec<(&'static str, u64)>,
    pub ssd_free: Option<u64>,
    pub smart: String,
    pub span: Rc<Cell<Option<(f32, f32)>>>,
    pub dragging: bool,
    /// The big size value is a text field being typed in.
    pub editing: bool,
    /// Set by each draw of the size card; `settle_focus` applies the typed size once it's gone.
    pub painted: Cell<bool>,
}

impl CacheView {
    pub fn new() -> Self {
        let root = crate::cache::cache_root();
        let ssd_free = crate::services::library::drive_space(&root).map(|(free, _)| free);
        Self {
            root,
            budget_bytes: crate::cache::budget_bytes(),
            budget_note: crate::cache::budget_note(),
            used_bytes: 0,
            tiers: Vec::new(),
            ssd_free,
            smart: String::new(),
            span: Rc::new(Cell::new(None)),
            dragging: false,
            editing: false,
            painted: Cell::new(false),
        }
    }

    /// Re-reads usage from disk (cheap: directory sizes of the cache folders).
    pub fn refresh(&mut self) {
        let usage = crate::cache::usage();
        self.used_bytes = usage.iter().map(|(_, b)| *b).sum();
        self.tiers = usage;
        self.budget_bytes = crate::cache::budget_bytes();
        self.budget_note = crate::cache::budget_note();
        self.ssd_free = crate::services::library::drive_space(&self.root).map(|(free, _)| free);
        self.smart = crate::cache::smart_status();
    }

    /// The largest size that can be typed: what's free on the drive plus what the cache
    /// already holds, or [`MAX_TYPED_GB`] when the drive can't be read.
    fn max_typed_bytes(&self) -> u64 {
        let cap = (MAX_TYPED_GB * GB) as u64;
        self.ssd_free.map_or(cap, |free| free.saturating_add(self.used_bytes).min(cap))
    }

    /// Snaps a window x on the slider to a stop and shows it; returns true when it moved.
    /// Nothing is applied until [`Self::commit`]: each applied size evicts down to it, so a
    /// sweep across the small stops must not empty the cache on the way.
    pub fn drag_to(&mut self, window_x: f32) -> bool {
        if self.budget_note.is_some() {
            return false;
        }
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
        if self.save_wanted(crate::cache::budget_bytes()) {
            crate::cache::set_budget_bytes(self.budget_bytes);
        }
        self.refresh();
    }

    /// The shown size differs from `current` and may be saved (never while it is read-only).
    fn save_wanted(&self, current: u64) -> bool {
        self.budget_note.is_none() && self.budget_bytes != current
    }
}

/// Smallest typed size, the slider's first stop.
pub const MIN_TYPED_GB: f64 = 1.0;
/// Upper bound when free space is unknown.
const MAX_TYPED_GB: f64 = 4096.0;

/// Reads a typed size in GB ("7", "7.5", "7,5", " 12 "). None for empty, zero or junk.
pub fn parse_gb(text: &str) -> Option<f64> {
    let gb = text.trim().replace(',', ".").parse::<f64>().ok()?;
    (gb.is_finite() && gb > 0.0).then_some(gb)
}

/// A typed size as budget bytes: rounded to 0.1 GB, then held between 1 GB and `max_bytes`.
/// The flag says the value was moved to a bound.
pub fn typed_budget(gb: f64, max_bytes: u64) -> (u64, bool) {
    let max_gb = (max_bytes as f64 / GB * 10.0).floor() / 10.0;
    let max_gb = max_gb.max(MIN_TYPED_GB);
    let rounded = (gb * 10.0).round() / 10.0;
    let held = rounded.clamp(MIN_TYPED_GB, max_gb);
    ((held * GB).round() as u64, held != rounded)
}

/// Knob position (0..=1) for any size: stops sit evenly on the track and sizes between two
/// stops sit proportionally between them, so a typed 7 GB lands between 5 and 10.
pub fn slider_fraction(bytes: u64) -> f32 {
    let gb = bytes as f64 / GB;
    let last = CACHE_STOPS_GB.len() - 1;
    if gb <= CACHE_STOPS_GB[0] {
        return 0.0;
    }
    for i in 0..last {
        let (lo, hi) = (CACHE_STOPS_GB[i], CACHE_STOPS_GB[i + 1]);
        if gb <= hi {
            return ((i as f64 + (gb - lo) / (hi - lo)) / last as f64) as f32;
        }
    }
    1.0
}

impl Root {
    /// Turns the big size value into a text field holding the current size.
    pub fn begin_cache_edit(&mut self, window: &mut Window) {
        if let Some(why) = self.cache.budget_note.clone() {
            self.show_toast(ToastKind::Bad, "Cache size can't be changed right now", Some(why));
            return;
        }
        self.cache.editing = true;
        self.cache.painted.set(true);
        self.cache_field.set(gb_label(self.cache.budget_bytes).trim_end_matches(" GB").to_owned());
        window.focus(&self.cache_field.focus);
    }

    /// Enter or a click elsewhere applies the typed size; Escape keeps the old one.
    pub fn end_cache_edit(&mut self, apply: bool, window: &mut Window) {
        if !self.cache.editing {
            return;
        }
        self.cache.editing = false;
        if self.cache_field.is_focused(window) {
            window.focus(&self.focus);
        }
        if apply {
            self.apply_typed_cache();
        }
    }

    /// Applies the typed size (also from `flush_on_close`, where no window is left to focus).
    pub fn apply_typed_cache(&mut self) {
        self.cache.editing = false;
        let Some(gb) = parse_gb(&self.cache_field.value) else {
            if !self.cache_field.value.trim().is_empty() {
                self.show_toast(ToastKind::Bad, "Cache size not changed", Some("Type a size in GB, like 7 or 7.5".to_owned()));
            }
            return;
        };
        let max = self.cache.max_typed_bytes();
        let (bytes, held) = typed_budget(gb, max);
        if held {
            let detail = if bytes as f64 <= MIN_TYPED_GB * GB + 1.0 {
                format!("The smallest cache is {}", gb_label(bytes))
            } else {
                format!("{} is the most this drive has room for", gb_label(bytes))
            };
            self.show_toast(ToastKind::Good, format!("Cache size set to {}", gb_label(bytes)), Some(detail));
        }
        self.cache.budget_bytes = bytes;
        self.cache.commit();
    }

    pub fn render_storage_section(&mut self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.cache.painted.set(true);
        let view = &self.cache;
        let stops = CACHE_STOPS_GB.len() - 1;
        let fraction = slider_fraction(view.budget_bytes);
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
            .child(crate::trace::target("cache-slider"))
            .child(div().absolute().top(px(7.)).left(relative(fraction)).ml(px(-7.)).size(px(14.)).rounded(px(7.)).bg(c(theme::TEXT)).child(crate::trace::target("cache-knob")))
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

        // The big size: click it to type a size instead of dragging.
        let size_value = if view.editing {
            div()
                .flex_none()
                .w(px(96.))
                .h(px(30.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(4.))
                .rounded(px(6.))
                .bg(c(theme::BASE))
                .border_1()
                .border_color(c(theme::BORDER_STRONG))
                .child(self.cache_field.render("cache-size-input", window).flex_1().justify_end().on_click(cx.listener(|this, _, window, cx| {
                    this.cache_field.deselect();
                    window.focus(&this.cache_field.focus);
                    cx.notify();
                })))
                .child(div().text_size(px(13.)).text_color(c(theme::TERTIARY)).child("GB"))
                .into_any_element()
        } else {
            div()
                .child(crate::trace::target("cache-size-value"))
                .id("cache-size-value")
                .flex_none()
                .px(px(6.))
                .mr(px(-6.))
                .rounded(px(6.))
                .border_1()
                .border_color(c(theme::RAISED))
                .cursor_text()
                .hover(|d| d.border_color(c(theme::BORDER)))
                .font_family(theme::display())
                .text_size(px(18.))
                .font_weight(FontWeight::BOLD)
                .child(gb_label(budget))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.begin_cache_edit(window);
                    cx.notify();
                }))
                .into_any_element()
        };

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
                    .child(size_value),
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
                    .when_some(view.budget_note.clone(), |d, why| d.child(div().text_size(px(11.)).text_color(c(theme::ERROR)).child(format!("Size is read-only for now: {why}"))))
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

#[cfg(test)]
#[path = "storage_settings_tests.rs"]
mod tests;
