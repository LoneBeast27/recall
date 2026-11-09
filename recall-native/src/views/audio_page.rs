//! Audio page: the recorded sources as strips side by side (a type icon and name, a device or
//! app picker, a live meter with a gain fader, record-in-clips and remove), a "+ Add source"
//! tile, and the MIDI console below. Sources are the controller's `recording.audio_sources`;
//! every change writes the whole list back and the recorder restarts after any save in flight.
//! Device and app lists are read each time the page opens, off the UI thread; meters run only
//! while the page shows.
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, DragMoveEvent, KeyDownEvent, Window, div, prelude::*, px};

use crate::{
    root::{Root, ToastKind},
    services::{
        audio_apps::{self, AppList},
        audio_devices::{self, DeviceLists},
        audio_sources::{self as sources, Kind, Source},
        meters::{Meters, spec_for},
        midi::Midi,
    },
    theme::{self, c},
    ui::{FieldEvent, TextField},
    views::{
        audio_strip::{STRIP_GAP, STRIP_W, StripDrag},
        pages::card,
    },
};

/// How long a removed source can be brought back.
pub const UNDO_FOR: Duration = Duration::from_secs(10);

/// A fader track's window top and height, written while it paints.
pub type FaderSpan = Rc<Cell<Option<(f32, f32)>>>;

/// Which menu is open on the Audio page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StripMenu {
    /// A strip's device or app picker, by source id.
    Device(String),
    Icon(String),
    /// "+ Add source".
    Add,
    /// "+ Add source" → Application audio.
    AddApp,
    /// "+ Add source" → Output.
    AddOutput,
}

/// Audio page state for the launch.
#[derive(Default)]
pub struct AudioPage {
    /// Lists were requested for this visit (cleared when the page closes).
    pub loaded: bool,
    pub devices: Option<DeviceLists>,
    pub apps: Option<AppList>,
    pub menu: Option<StripMenu>,
    /// The source whose name is being edited, and its field.
    pub renaming: Option<String>,
    pub name_field: Option<TextField>,
    /// The fader being dragged: source id and the gain shown.
    pub fader: Option<(String, f32)>,
    /// A gain let go and sent to the recorder, shown until the saved list carries it.
    pub pending_gain: Option<(String, f32, Instant)>,
    /// Each fader track's window top and height, written while painting.
    pub spans: HashMap<String, FaderSpan>,
    /// The last removed source, its place and when.
    pub removed: Option<(usize, Source, Instant)>,
    /// "+ Add source" was opened from the card header, not the tile.
    pub add_in_header: bool,
    /// A strip being dragged by its handle: its id and the slot it would drop into.
    pub reorder: Option<(String, usize)>,
    /// Each strip's window left edge, written while painting (places a dragged strip).
    pub lefts: HashMap<String, Rc<Cell<Option<f32>>>>,
    /// The source whose "Muted by the mic hotkey" untick was refused, and the note why (no
    /// endpoint id to save, or an identical device shares the name): its strip says so until a
    /// device is picked or the page closes (R6/R7-deep-audio-1).
    pub mute_refused: Option<(String, &'static str)>,
}

impl AudioPage {
    /// The page closed: lists are read again and menus close next time.
    pub fn closed(&mut self) {
        self.loaded = false;
        self.menu = None;
        self.renaming = None;
        self.fader = None;
        self.reorder = None;
        self.mute_refused = None;
    }

    pub fn span(&mut self, id: &str) -> FaderSpan {
        self.spans.entry(id.to_owned()).or_default().clone()
    }

    /// An app icon by exe, when the list has one.
    pub fn icon(&self, exe: &str) -> Option<std::sync::Arc<gpui::RenderImage>> {
        self.apps.as_ref()?.icons.get(&exe.trim().to_lowercase()).cloned()
    }

    pub fn left(&mut self, id: &str) -> Rc<Cell<Option<f32>>> {
        self.lefts.entry(id.to_owned()).or_default().clone()
    }

    pub fn undo_open(&self) -> bool {
        self.removed.as_ref().is_some_and(|(_, _, at)| at.elapsed() < UNDO_FOR)
    }
}

impl Root {
    /// The saved sources (empty until the recorder has sent its config).
    pub(crate) fn audio_sources(&self) -> Vec<Source> {
        sources::from_config(self.config_value("recording", "audio_sources"))
    }

    fn save_sources(&mut self, list: &[Source]) {
        self.set_config("recording", "audio_sources", sources::to_config(list));
    }

    /// Change one source by id and save.
    fn edit_source(&mut self, id: &str, edit: impl FnOnce(&mut Source)) {
        let mut list = self.audio_sources();
        if let Some(source) = list.iter_mut().find(|s| s.id == id) {
            edit(source);
            self.save_sources(&list);
        }
    }

    /// A strip's device: its name and, for an input, its endpoint id (a line-in skips Alt+M only
    /// on that endpoint, R6-deep-audio-1). A line-in moved onto an endpoint Windows lists as a
    /// microphone gets Alt+M back on, with a toast (R5-deep-audio-2); the device is looked up in
    /// the lists at the click (R6-deep-audio-4).
    pub(crate) fn set_source_device(&mut self, id: &str, device: String, device_id: String) {
        let lists = self.settings_ui.audio.devices.clone();
        let mut rearmed = false;
        if self.settings_ui.audio.mute_refused.as_ref().is_some_and(|(s, _)| s == id) {
            self.settings_ui.audio.mute_refused = None;
        }
        self.edit_source(id, |s| {
            if let Some(on) = crate::services::audio_mute::device_pick_effect(s, lists.as_ref(), (&device, &device_id)) {
                s.mic_mute = Some(on);
                rearmed = true;
            }
            s.device = device;
            s.device_id = device_id;
        });
        if rearmed {
            self.show_toast(ToastKind::Info, "Muted by the mic hotkey", Some(crate::services::audio_mute::REARMED.into()));
        }
    }

    /// Whether the mic-mute hotkey silences this input (off: a line-in or capture card). Off
    /// saves the endpoint id too, as a line-in skips Alt+M only there; with no id to save it is
    /// refused and the strip says so (R6-deep-audio-1).
    pub(crate) fn set_source_mic_mute(&mut self, id: &str, on: bool) {
        let source = self.audio_sources().into_iter().find(|s| s.id == id);
        let effect = match source {
            Some(s) if !on => crate::services::audio_mute::mic_toggle_effect(&s, self.settings_ui.audio.devices.as_ref()),
            _ => Ok((true, String::new())),
        };
        match effect {
            Ok((on, device_id)) => {
                self.settings_ui.audio.mute_refused = None;
                self.edit_source(id, |s| {
                    s.mic_mute = Some(on);
                    if !on {
                        s.device_id = device_id;
                    }
                });
            }
            Err(why) => self.settings_ui.audio.mute_refused = Some((id.to_owned(), why)),
        }
    }

    pub(crate) fn set_source_icon(&mut self, id: &str, icon: String) {
        self.edit_source(id, |source| {
            if !source.is_primary() {
                source.icon = icon;
            }
        });
    }

    /// Point an app source at another app; a name that was the old app's follows the new one.
    pub(crate) fn set_source_app(&mut self, id: &str, exe: String, label: String) {
        let old = self.audio_page_app_label(id);
        self.edit_source(id, |s| {
            if old.as_deref().is_some_and(|o| o == s.name) || s.name == Kind::App.default_name() {
                s.name = sources::clip_name(&label);
            }
            s.exe = exe;
        });
    }

    fn audio_page_app_label(&self, id: &str) -> Option<String> {
        let source = self.audio_sources().into_iter().find(|s| s.id == id)?;
        let apps = self.settings_ui.audio.apps.as_ref()?;
        Some(apps.apps.iter().find(|a| a.exe.eq_ignore_ascii_case(&source.exe)).map_or_else(|| audio_apps::exe_stem(&source.exe), |a| a.label.clone()))
    }

    pub(crate) fn toggle_source(&mut self, id: &str) {
        self.edit_source(id, |s| s.enabled = !s.enabled);
    }

    pub(crate) fn set_source_gain(&mut self, id: &str, gain_db: f32) {
        let gain_db = gain_db.clamp(sources::GAIN_MIN_DB, sources::GAIN_MAX_DB);
        self.settings_ui.audio.pending_gain = Some((id.to_owned(), gain_db, Instant::now()));
        self.edit_source(id, |s| s.gain_db = gain_db);
    }

    /// The gain a strip shows now (see `sources::shown_gain`); forgets a pending gain once the
    /// saved list has it or it has waited too long.
    pub(crate) fn strip_gain(&mut self, source: &Source) -> f32 {
        let page = &mut self.settings_ui.audio;
        let dragging = page.fader.as_ref().filter(|(f, _)| *f == source.id).map(|(_, g)| *g);
        let pending = page.pending_gain.as_ref().filter(|(p, _, _)| *p == source.id).map(|(_, g, at)| (*g, at.elapsed()));
        let gain = sources::shown_gain(dragging, pending, source.gain_db);
        if pending.is_some() && dragging.is_none() && gain == source.gain_db {
            page.pending_gain = None;
        }
        gain
    }

    /// The dragged strip moved: work out the slot under the pointer and show the list there.
    fn drag_strip(&mut self, id: &str, x: f32) -> bool {
        let list = self.audio_sources();
        let page = &mut self.settings_ui.audio;
        let left = list.iter().filter_map(|s| page.lefts.get(&s.id).and_then(|l| l.get())).fold(f32::INFINITY, f32::min);
        if !left.is_finite() {
            return false;
        }
        let slot = sources::drop_slot(x, left, STRIP_W + STRIP_GAP, list.len());
        let next = Some((id.to_owned(), slot));
        let changed = page.reorder != next;
        page.reorder = next;
        changed
    }

    /// The dragged strip was let go over the Sources card: save the new order.
    fn drop_strip(&mut self) {
        let Some((id, to)) = self.settings_ui.audio.reorder.take() else { return };
        let list = self.audio_sources();
        let next = sources::moved(&list, &id, to);
        if next != list {
            self.save_sources(&next);
        }
    }

    pub(crate) fn add_source(&mut self, kind: Kind, exe: &str, name: &str) {
        match sources::with_new(&self.audio_sources(), kind, exe, name) {
            Some(list) => {
                if matches!(kind, Kind::Input | Kind::Output) {
                    self.settings_ui.audio.menu = list.last().map(|s| StripMenu::Device(s.id.clone()));
                }
                self.save_sources(&list);
            }
            None => self.show_toast(ToastKind::Info, "Can't add that source", Some(format!("Up to {} sources, one of them the game.", sources::SOURCES_MAX))),
        }
    }

    /// Remove at once; the toast offers Undo for `UNDO_FOR`.
    pub(crate) fn remove_source(&mut self, id: &str) {
        let mut list = self.audio_sources();
        let Some(index) = list.iter().position(|s| s.id == id) else { return };
        let removed = list.remove(index);
        self.save_sources(&list);
        self.show_toast(ToastKind::Info, format!("{} removed", removed.name), Some("Its track is no longer recorded.".into()));
        if let Some(toast) = self.toast.as_mut() {
            toast.until = Instant::now() + UNDO_FOR;
        }
        self.settings_ui.audio.removed = Some((index, removed, Instant::now()));
    }

    pub(crate) fn undo_remove_source(&mut self) {
        let Some((index, source, _)) = self.settings_ui.audio.removed.take() else { return };
        let name = source.name.clone();
        match sources::restored(&self.audio_sources(), index, source) {
            Some(list) => {
                self.save_sources(&list);
                self.toast = None;
            }
            // Never a silent no-op with the Undo toast still up.
            None => self.show_toast(ToastKind::Info, format!("Can't bring back {name}"), Some(format!("Up to {} sources, one of them the game.", sources::SOURCES_MAX))),
        }
    }

    pub(crate) fn start_rename(&mut self, id: &str, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let page = &mut self.settings_ui.audio;
        let field = page.name_field.get_or_insert_with(|| TextField::new(cx, "Source name", sources::NAME_MAX));
        field.set(name);
        window.focus(&field.focus);
        page.renaming = Some(id.to_owned());
    }

    /// End a rename: save a non-blank name, then give the keyboard back to the page.
    pub(crate) fn end_source_rename(&mut self, save: bool, window: &mut Window) {
        let Some(id) = self.settings_ui.audio.renaming.take() else { return };
        let name = self.settings_ui.audio.name_field.as_ref().map(|f| sources::clip_name(&f.value)).unwrap_or_default();
        if save && !name.is_empty() {
            self.edit_source(&id, |s| s.name = name);
        }
        window.focus(&self.focus);
    }

    /// Keys for the name field while a source is renamed. True when the press was taken.
    pub(crate) fn audio_name_key(&mut self, event: &KeyDownEvent, clipboard: Option<String>, window: &mut Window) -> bool {
        let page = &mut self.settings_ui.audio;
        if page.renaming.is_none() {
            return false;
        }
        let Some(field) = page.name_field.as_mut().filter(|f| f.is_focused(window)) else { return false };
        match field.key_down(event, clipboard) {
            FieldEvent::Submit => self.end_source_rename(true, window),
            FieldEvent::Cancel => self.end_source_rename(false, window),
            FieldEvent::Changed | FieldEvent::None => {}
        }
        true
    }

    /// Escape while a strip is dragged: end the drag and keep the order. True when the press was taken.
    /// Ending gpui's drag means letting go of the button drops nothing, so nothing is saved.
    /// StripDrag is the only gpui drag in the app, so any active drag here is a strip.
    pub(crate) fn audio_drag_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let k = &event.keystroke;
        let dragging = self.settings_ui.audio.reorder.is_some() || cx.has_active_drag();
        if !sources::escape_cancels_drag(&k.key, k.modifiers.modified(), dragging) {
            return false;
        }
        cx.stop_active_drag(window);
        self.settings_ui.audio.reorder = None;
        true
    }

    /// Read the device and app lists once per visit, off the UI thread.
    fn load_audio_lists(&mut self, cx: &mut Context<Self>) {
        if self.settings_ui.audio.loaded {
            return;
        }
        self.settings_ui.audio.loaded = true;
        if crate::TESTING {
            let (devices, apps) = super::setup_testing::audio_lists();
            self.settings_ui.audio.devices = Some(devices);
            self.settings_ui.audio.apps = Some(apps);
            return;
        }
        let saved: Vec<String> = self.audio_sources().into_iter().filter(|s| s.kind == Kind::App).map(|s| s.exe).collect();
        cx.spawn(async move |this, cx| {
            let (devices, apps) = cx
                .background_executor()
                .spawn(async move { (audio_devices::enumerate_blocking(), audio_apps::list_with_icons_blocking(&saved)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.settings_ui.audio.devices = Some(devices);
                this.settings_ui.audio.apps = Some(apps);
                cx.notify();
            });
        })
        .detach();
    }

    /// Start or replace the meters and the MIDI monitor so they follow the settings: one meter
    /// per source, and the chosen MIDI input ("" = none).
    pub(crate) fn sync_audio_monitors(&mut self) {
        if crate::TESTING {
            return;
        }
        let game_pid = self.snap.session.as_ref().and_then(|s| s.target.as_ref()).map(|t| t.pid).filter(|&pid| pid != 0);
        let specs: Vec<_> = self.audio_sources().iter().map(|s| spec_for(s, game_pid)).collect();
        if self.meters.as_ref().is_none_or(|m| m.specs() != specs.as_slice()) {
            self.meters = Some(Meters::spawn(specs));
        }
        let port = self.config_value("midi", "input").and_then(serde_json::Value::as_str).unwrap_or("").trim().to_owned();
        if port.is_empty() {
            self.midi = None;
        } else if self.midi.as_ref().is_none_or(|m| m.wanted() != port) {
            self.midi = Some(Midi::spawn(port));
        }
    }

    pub fn render_audio(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.sync_audio_monitors();
        self.load_audio_lists(cx);
        let mut body = Vec::new();
        body.extend(self.offline_banner(cx));
        body.push(self.sources_card(window, cx));
        if !crate::TESTING {
            body.push(self.midi_card(cx));
        }
        let subtitle = if crate::TESTING {
            "Try adding or routing demo audio sources."
        } else if self.setup.is_some() {
            "New profiles use your Windows microphone and playback defaults. A separate communications device gets its own track."
        } else {
            "Sources and MIDI"
        };
        self.page_shell("Audio", subtitle, body)
    }

    fn sources_card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let list = self.audio_sources();
        let readings = self.meters.as_ref().map(|m| m.snapshot()).unwrap_or_default();
        let shown = self.meter_ballistics(&readings);
        let levels: HashMap<String, (f32, f32, bool)> = readings
            .iter()
            .zip(shown)
            .map(|(r, (rms, hold))| (r.label.clone(), (rms, hold, r.state == crate::services::meters::MeterState::Live)))
            .collect();
        let recorded = list.iter().filter(|s| s.enabled).count();
        let meta = match recorded {
            0 => "No tracks recorded".to_owned(),
            1 => "1 track in clips".to_owned(),
            n => format!("{n} tracks in clips"),
        };
        // While a strip is dragged the row shows where it would land; a drag let go outside
        // the card changes nothing.
        if self.settings_ui.audio.reorder.is_some() && !cx.has_active_drag() {
            self.settings_ui.audio.reorder = None;
        }
        let shown_order = match &self.settings_ui.audio.reorder {
            Some((id, to)) => sources::moved(&list, id, *to),
            None => list.clone(),
        };
        let mut strips: Vec<AnyElement> = shown_order.iter().map(|s| self.render_strip(s, levels.get(&s.id).copied(), window, cx)).collect();
        if !sources::addable(&list).is_empty() {
            strips.push(self.render_add_tile(&list, cx));
        }
        let note = if self.snap.config.is_none() {
            "Start the recorder to change sources."
        } else {
            "Each source is one track in your clips, named as shown, left to right. Drag a strip by its dots to reorder. Changes apply after the current save."
        };
        card()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(div().flex_1().text_size(px(13.)).line_height(px(18.)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(theme::TEXT)).child("Sources"))
                    .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(meta))
                    .when(!sources::addable(&list).is_empty(), |d| d.child(self.render_add_button(&list, cx))),
            )
            // One row that scrolls sideways when the strips outgrow the page.
            .child(
                div()
                    .id("audio-strips")
                    .flex()
                    .gap(px(STRIP_GAP))
                    .pb(px(6.))
                    .overflow_x_scroll()
                    .on_drag_move(cx.listener(|this, e: &DragMoveEvent<StripDrag>, _, cx| {
                        let id = e.drag(cx).id.clone();
                        if this.drag_strip(&id, f32::from(e.event.position.x)) {
                            cx.notify();
                        }
                    }))
                    .children(strips),
            )
            .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(note))
            .on_drop(cx.listener(|this, _: &StripDrag, _, cx| {
                this.drop_strip();
                cx.notify();
            }))
            .into_any_element()
    }
}
