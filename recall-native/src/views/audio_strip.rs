//! One Audio page strip (Voicemeeter / OBS mixer style), top to bottom: type icon and name,
//! the device or app picker, a tall meter with the gain fader beside it, then record-in-clips
//! and remove. A row of dots on top drags the strip to another place (the list order is the
//! clips' track order). The last tile adds a source. Levels shown include the source's gain.
use gpui::{AnyElement, Context, CursorStyle, Div, FontWeight, ImageSource, MouseButton, MouseDownEvent, SharedString, Window, canvas, div, img, prelude::*, px, relative};

use crate::{
    root::Root,
    services::{
        audio_sources::{self as sources, Kind, Source},
        audio_mute::shown_level,
        meters::{bar_fraction, with_gain},
    },
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    views::audio_page::StripMenu,
};

pub const STRIP_W: f32 = 156.;
/// Space between strips in the row.
pub const STRIP_GAP: f32 = 12.;
const METER_H: f32 = 176.;
/// Level zones like a broadcast mixer: green to -20 dB, yellow to -9 dB, red above.
const ZONES: [(f32, f32, u32); 3] = [(-60., -20., theme::ACTIVE), (-20., -9., theme::WARNING), (-9., 0., theme::ERROR)];

fn kind_icon(kind: Kind) -> Icon {
    match kind {
        Kind::Game => Icon::Gamepad,
        Kind::Output => Icon::Volume,
        Kind::Input => Icon::Mic,
        Kind::App => Icon::AudioLines,
    }
}

/// The vertical level meter: zoned fill to `rms_db`, a peak-hold line at `hold_db`.
fn meter(rms_db: f32, hold_db: f32, live: bool) -> Div {
    let fill = bar_fraction(rms_db);
    let mut bar = div().w(px(10.)).h(px(METER_H)).relative().rounded(px(3.)).bg(c(theme::BASE)).border_1().border_color(c(theme::BORDER_SUBTLE));
    if live {
        for (lo, hi, color) in ZONES {
            let (from, to) = (bar_fraction(lo), bar_fraction(hi));
            let height = (fill.min(to) - from).max(0.0);
            if height > 0.0 {
                bar = bar.child(div().absolute().left_0().right_0().bottom(relative(from)).h(relative(height)).bg(c(color)));
            }
        }
        let hold = bar_fraction(hold_db);
        if hold > 0.0 {
            let color = if hold_db > -1.0 { theme::SIGNAL } else { theme::TEXT };
            bar = bar.child(div().absolute().left_0().right_0().bottom(relative(hold)).h(px(2.)).bg(c(color)));
        }
    }
    bar
}

/// A strip on the move: what the pointer carries, drawn as a small card under it.
#[derive(Clone)]
pub struct StripDrag {
    pub id: String,
    name: String,
    kind: Kind,
}

impl Render for StripDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(STRIP_W))
            .h(px(40.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(8.))
            .bg(c(theme::FLOATING))
            .border_1()
            .border_color(c(theme::BORDER_STRONG))
            .font_family(theme::sans())
            .text_size(px(13.))
            .text_color(c(theme::TEXT))
            .font_weight(FontWeight::MEDIUM)
            .child(icon(Icon::Grip, 14., theme::MUTED))
            .child(icon(kind_icon(self.kind), 16., theme::MUTED))
            .child(div().flex_1().min_w_0().truncate().child(self.name.clone()))
    }
}

impl Root {
    /// The picker label: the device (or "Windows default (…)"), the app, or a missing one.
    /// The warning under a strip's device button: a refused "Muted by the mic hotkey" untick
    /// (R6-deep-audio-1), else `strip_note`.
    fn strip_warning(&self, source: &Source) -> Option<String> {
        let audio = &self.settings_ui.audio;
        if let Some((_, why)) = audio.mute_refused.as_ref().filter(|(id, _)| *id == source.id) {
            return Some((*why).to_owned());
        }
        crate::services::audio_sources::strip_note(source, audio.devices.as_ref())
    }

    fn source_label(&self, source: &Source) -> String {
        let page = &self.settings_ui.audio;
        match source.kind {
            Kind::App => page
                .apps
                .as_ref()
                .and_then(|a| a.apps.iter().find(|p| p.exe.eq_ignore_ascii_case(&source.exe)))
                .map_or_else(|| format!("{} (not running)", crate::services::audio_apps::exe_stem(&source.exe)), |p| p.label.clone()),
            kind => {
                let lists = page.devices.as_ref();
                let default = match lists {
                    Some(l) if kind.uses_inputs() => l.default_input.as_deref(),
                    Some(l) => l.default_output.as_deref(),
                    None => None,
                };
                crate::services::audio_pick::source_device_label(source, lists, default)
            }
        }
    }

    pub(crate) fn render_strip(&mut self, source: &Source, level: Option<(f32, f32, bool)>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let id = source.id.clone();
        let renaming = self.settings_ui.audio.renaming.as_deref() == Some(id.as_str());
        let gain = self.strip_gain(source);
        let (rms, hold, live) = level.unwrap_or((-90., -90., false));
        // Alt+M on and this source muted by it: flat, as the clip records it (R5-deep-audio-5).
        let session_muted = self.snap.session.as_ref().is_some_and(|s| s.mic_muted) || self.snap.engine.as_ref().is_some_and(|e| e.mic_muted);
        let mutes = sources::mutes(source, self.settings_ui.audio.devices.as_ref());
        let muted = session_muted && mutes;
        // Bar, hold line and number all after the gain, so they move together with the fader.
        let (rms, hold) = (shown_level(with_gain(rms, gain), session_muted, mutes), shown_level(with_gain(hold, gain), session_muted, mutes));
        let level_text = if !live { "—".to_owned() } else if muted { "Muted".to_owned() } else if rms <= -89.0 { "-∞".to_owned() } else { format!("{:.0} dB", rms) };

        let type_icon: AnyElement = match self.settings_ui.audio.icon(&source.exe).filter(|_| source.kind == Kind::App) {
            Some(image) => img(ImageSource::Render(image)).size(px(16.)).flex_none().into_any_element(),
            None => icon(kind_icon(source.kind), 16., theme::MUTED).into_any_element(),
        };
        let name: AnyElement = if renaming {
            match self.settings_ui.audio.name_field.as_ref() {
                Some(field) => field.render(SharedString::from(format!("name-{id}")), window).flex_1().min_w_0().into_any_element(),
                None => div().into_any_element(),
            }
        } else {
            let (rename_id, rename_to) = (id.clone(), source.name.clone());
            div()
                .id(SharedString::from(format!("strip-name-{id}")))
                .flex_1()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .cursor_text()
                .child(source.name.clone())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.start_rename(&rename_id, &rename_to, window, cx);
                    cx.notify();
                }))
                .into_any_element()
        };
        // The drag handle: six dots across the top of the strip.
        let drag = StripDrag { id: id.clone(), name: source.name.clone(), kind: source.kind };
        let grip = div()
            .id(SharedString::from(format!("strip-grip-{id}")))
            .h(px(14.))
            .mt(px(-4.))
            .mb(px(-4.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .cursor(CursorStyle::OpenHand)
            .hover(|s| s.bg(c(theme::FLOATING)))
            .child(icon(Icon::Grip, 16., theme::TERTIARY))
            .child(crate::trace::target(format!("strip-grip-{id}")))
            .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()));
        let header = div().h(px(26.)).flex().items_center().gap(px(8.)).child(type_icon).child(name);

        let menu_open = self.settings_ui.audio.menu == Some(StripMenu::Device(id.clone()));
        let label = self.source_label(source);
        // A saved device that is gone records silence until it is back (its meter shows nothing).
        // Every strip keeps this line so meters line up (a line-in on the default wraps it).
        let missing = div()
            .min_h(px(14.))
            .text_size(px(11.))
            .line_height(px(14.))
            .text_color(c(theme::WARNING))
            .when_some(self.strip_warning(source), |d, note| d.child(note));
        let picker_id = id.clone();
        let picker = div()
            .relative()
            .child(
                ui::button(SharedString::from(format!("strip-device-{id}")), Variant::Ghost)
                    .w_full()
                    .justify_between()
                    .border_1()
                    .border_color(c(theme::BORDER))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).child(label))
                    .child(icon(Icon::ChevronDown, 12., theme::MUTED))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let menu = StripMenu::Device(picker_id.clone());
                        this.settings_ui.audio.menu = if this.settings_ui.audio.menu.as_ref() == Some(&menu) { None } else { Some(menu) };
                        cx.notify();
                    })),
            )
            .when(menu_open, |d| d.child(self.device_menu_for(source, cx)))
            .child(missing);

        let body = div()
            .flex()
            .gap(px(10.))
            .justify_center()
            .when(!source.enabled, |d| d.opacity(0.4))
            .child(div().flex().flex_col().items_center().gap(px(6.)).child(meter(rms, hold, live && source.enabled)).child(div().text_size(px(11.)).font_family(theme::mono()).text_color(c(theme::TERTIARY)).child(level_text)))
            .child(self.render_fader(&id, gain, cx));

        let (toggle_id, remove_id) = (id.clone(), id.clone());
        // One box in both states (only the icon, words and colour change), with room for the
        // longer "Not recorded" so the button never overflows and shifts.
        let footer = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(
                ui::button(SharedString::from(format!("strip-rec-{id}")), Variant::Secondary)
                    .flex_1()
                    .flex_shrink()
                    .min_w_0()
                    .px(px(4.))
                    .gap(px(4.))
                    .text_size(px(12.))
                    .when(!source.enabled, |b| b.bg(c(theme::BASE)).text_color(c(theme::MUTED)))
                    .child(icon(if source.enabled { Icon::Volume } else { Icon::VolumeX }, 13., if source.enabled { theme::TEXT } else { theme::MUTED }))
                    .child(div().min_w_0().truncate().child(if source.enabled { "In clips" } else { "Not recorded" }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_source(&toggle_id);
                        cx.notify();
                    })),
            )
            .child(ui::icon_button(SharedString::from(format!("strip-remove-{id}")), Icon::X, 24., theme::MUTED, false).on_click(cx.listener(move |this, _, _, cx| {
                this.remove_source(&remove_id);
                cx.notify();
            })));

        let moving = self.settings_ui.audio.reorder.as_ref().is_some_and(|(d, _)| *d == id);
        let left = self.settings_ui.audio.left(&id);
        div()
            .w(px(STRIP_W))
            .flex_none()
            .relative()
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .rounded(px(8.))
            .bg(c(theme::OVERLAY))
            .border_1()
            .border_color(c(theme::BORDER_SUBTLE))
            // The empty place a dragged strip will drop into.
            .when(moving, |d| d.opacity(0.35).border_dashed().border_color(c(theme::BORDER_STRONG)))
            .child(canvas(move |b, _, _| left.set(Some(f32::from(b.origin.x))), |_, _, _, _| {}).absolute().inset_0())
            .child(grip)
            .child(header)
            .child(picker)
            .child(body)
            .child(footer)
            .into_any_element()
    }

    /// The gain fader: drag to set (-60..+12 dB), double-click for 0 dB; saved on release.
    /// Only the press is on the track: moves and the release are followed window-wide
    /// (`Root::drag_watch`), so a drag that leaves the track keeps going and ends anywhere.
    fn render_fader(&mut self, id: &str, gain: f32, cx: &mut Context<Self>) -> AnyElement {
        let span = self.settings_ui.audio.span(id);
        let fraction = sources::fader_fraction(gain);
        let unity = sources::fader_fraction(0.0);
        let at = |span: &crate::views::audio_page::FaderSpan, y: f32| span.get().map(|(top, h)| sources::gain_at(1.0 - (y - top) / h.max(1.0)));
        let (down_id, down_span) = (id.to_owned(), span.clone());
        let track = div()
            .id(SharedString::from(format!("fader-{id}")))
            .w(px(28.))
            .h(px(METER_H))
            .relative()
            .cursor_pointer()
            .child(canvas(move |b, _, _| span.set(Some((f32::from(b.origin.y), f32::from(b.size.height)))), |_, _, _, _| {}).absolute().inset_0())
            .child(div().absolute().top_0().bottom_0().left(px(12.)).w(px(4.)).rounded(px(2.)).bg(c(theme::TOP)))
            .child(div().absolute().left(px(6.)).w(px(16.)).h(px(1.)).bottom(relative(unity)).bg(c(theme::BORDER_STRONG)))
            .child(div().absolute().left(px(2.)).w(px(24.)).h(px(10.)).mb(px(-5.)).bottom(relative(fraction)).rounded(px(3.)).bg(c(theme::TEXT)))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                if e.click_count >= 2 {
                    this.settings_ui.audio.fader = None;
                    this.set_source_gain(&down_id, 0.0);
                } else if let Some(g) = at(&down_span, f32::from(e.position.y)) {
                    this.settings_ui.audio.fader = Some((down_id.clone(), g));
                }
                cx.notify();
            }));
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.))
            .child(track)
            .child(div().text_size(px(11.)).font_family(theme::mono()).text_color(c(theme::MUTED)).child(sources::gain_label(gain)))
            .into_any_element()
    }

    /// The dragged fader follows the pointer's height, wherever the pointer is.
    pub(crate) fn fader_to(&mut self, y: f32) {
        let page = &mut self.settings_ui.audio;
        let Some((id, _)) = page.fader.clone() else { return };
        if let Some(gain) = page.spans.get(&id).and_then(|s| s.get()).map(|(top, h)| sources::gain_at(1.0 - (y - top) / h.max(1.0))) {
            page.fader = Some((id, gain));
        }
    }

    /// The button came up: save the dragged gain.
    pub(crate) fn commit_dragged_fader(&mut self) {
        if let Some((id, gain)) = self.settings_ui.audio.fader.take() {
            self.set_source_gain(&id, gain);
        }
    }

    pub(crate) fn render_add_tile(&mut self, list: &[Source], cx: &mut Context<Self>) -> AnyElement {
        let page = &self.settings_ui.audio;
        let open = !page.add_in_header && matches!(page.menu, Some(StripMenu::Add | StripMenu::AddApp));
        // A column so the tile stretches to the strips' full height.
        div()
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("add-source")
                    .w(px(STRIP_W))
                    .flex_1()
                    .min_h(px(METER_H + 120.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_dashed()
                    .border_color(c(theme::BORDER_STRONG))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(theme::OVERLAY)))
                    .child(icon(Icon::Plus, 20., theme::MUTED))
                    .child(div().text_size(px(13.)).text_color(c(theme::MUTED)).child("Add source"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let page = &mut this.settings_ui.audio;
                        page.menu = if page.menu.is_some() { None } else { Some(StripMenu::Add) };
                        page.add_in_header = false;
                        cx.notify();
                    })),
            )
            .when(open, |d| d.child(self.add_menu(list, false, cx)))
            .into_any_element()
    }

    /// The Sources card's own "Add source" button (the tile can be scrolled out of view).
    pub(crate) fn render_add_button(&mut self, list: &[Source], cx: &mut Context<Self>) -> AnyElement {
        let page = &self.settings_ui.audio;
        let open = page.add_in_header && matches!(page.menu, Some(StripMenu::Add | StripMenu::AddApp));
        div()
            .relative()
            .child(ui::button("add-source-header", Variant::Ghost).border_1().border_color(c(theme::BORDER)).child(icon(Icon::Plus, 13., theme::TEXT)).child("Add source").on_click(cx.listener(|this, _, _, cx| {
                let page = &mut this.settings_ui.audio;
                page.menu = if page.menu.is_some() { None } else { Some(StripMenu::Add) };
                page.add_in_header = true;
                cx.notify();
            })))
            .when(open, |d| d.child(self.add_menu(list, true, cx)))
            .into_any_element()
    }
}
