//! The export page's Advanced area (`editor_inspector.rs`): choices set once and left alone,
//! closed by default. Export method, how tracks go into the file, each track's "Mix into main
//! track", an upscale for clips recorded below their game's size, and a custom frame rate. The closed header names any of these that is not the
//! default, so nothing set in here is hidden.
#[allow(unused_imports)]
use super::*;

impl Root {
    pub(super) fn render_export_advanced(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let lossless = self.ed.lossless();
        let mix_down = self.ed.mix_down();
        let custom = self.fps_custom && !lossless;
        let fps = self.ed.fps;
        let mut changed = Vec::new();
        if lossless {
            changed.push("Lossless trim".to_owned());
        }
        if !mix_down {
            changed.push("Separate tracks".to_owned());
        }
        let upscale = self.ed.upscale_height().filter(|_| !lossless);
        if let Some(height) = upscale {
            changed.push(format!("Upscaled to {height}p"));
        }
        if custom {
            changed.push(format!("{fps} fps"));
        }
        let open = self.export_advanced;
        let header = ui::disclosure("export-advanced", "Advanced", changed.join(" · "), open).on_click(cx.listener(|this, _, _, cx| {
            this.export_advanced = !this.export_advanced;
            cx.notify();
        }));
        let mut column = div().flex().flex_col().gap(px(12.)).child(header);
        if open {
            column = column.child(self.render_method(cx)).child(self.render_track_mixing(cx));
            if !lossless && !self.ed.upscale_choices().is_empty() {
                column = column.child(self.render_upscale(cx));
            }
            if !lossless {
                column = column.child(self.render_custom_fps(window, cx));
            }
        }
        column.into_any_element()
    }

    fn render_method(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut method = ui::segment_track();
        // Ids keep the old short labels: the check scripts click them.
        for (key, label, mode) in [("Re-encode", "Re-encode", ExportMode::Reencode), ("Lossless", "Lossless trim", ExportMode::Lossless)] {
            method = method.child(ui::segment(SharedString::from(format!("mode-{key}")), label, self.ed.export_settings.mode == mode).on_click(cx.listener(move |this, _, _, cx| {
                this.ed.set_export_mode(mode);
                cx.notify();
            })));
        }
        labelled("Method", method.into_any_element())
            .child(small_note("Lossless trim only cuts: it keeps the recording's frame rate, size and quality, in seconds."))
            .into_any_element()
    }

    fn render_track_mixing(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mix_down = self.ed.mix_down();
        let mut mixing = ui::segment_track();
        for (label, on) in [("Mix", true), ("Separate", false)] {
            mixing = mixing.child(
                ui::segment(SharedString::from(format!("mixdown-{label}")), label, mix_down == on)
                    .child(crate::trace::target(format!("mixdown-{label}")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ed.set_mix_down(on);
                        cx.notify();
                    })),
            );
        }
        let note = if mix_down {
            "Tracks switched on below become track 1; players that play one track hear them all."
        } else {
            "Every track is its own track in the file, nothing mixed."
        };
        let mut column = labelled("Tracks in the file", mixing.into_any_element()).child(small_note(note));
        let tracks = self.ed.info.as_ref().map(|i| i.audio.clone()).unwrap_or_default();
        for (index, track) in tracks.iter().enumerate() {
            let setting = self.ed.setting(track.stream_index).cloned();
            let muted = setting.as_ref().is_some_and(|s| s.muted);
            let in_mix = setting.as_ref().map_or_else(|| track.default_setting().mix, |s| s.mix);
            let label = format!("Mix {} into main track", track_name(&track.name, index));
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .when(muted || !mix_down, |d| d.opacity(0.4))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(c(theme::MUTED)).child(label))
                    .child(ui::switch(("mix", index), in_mix).child(crate::trace::target(format!("mix-switch-{index}"))).when(mix_down, |s| {
                        s.on_click(cx.listener(move |this, _, _, cx| {
                            this.ed.toggle_mix(index);
                            cx.notify();
                        }))
                    })),
            );
        }
        column.into_any_element()
    }

    /// Only for a clip recorded below its game's size (Settings > Recording > Resolution): the
    /// one way an export goes above the recording, and never above the game's own size.
    fn render_upscale(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let picked = self.ed.upscale_height();
        let mut sizes = ui::segment_track();
        let choices = std::iter::once(None).chain(self.ed.upscale_choices().into_iter().map(Some));
        for value in choices {
            let key = value.map_or("Off".to_owned(), |h| h.to_string());
            let label = value.map_or("Off".to_owned(), |h| format!("{h}p"));
            let id = format!("upscale-{key}");
            sizes = sizes.child(ui::segment(SharedString::from(id.clone()), label, picked == value).child(crate::trace::target(id)).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.ed.set_upscale_height(value);
                    cx.notify();
                },
            )));
        }
        let info = self.ed.info.as_ref();
        let (recorded, native) = (info.map_or(0, |i| i.height), info.and_then(|i| i.native_height).unwrap_or(0));
        let mut column = labelled("Upscale", sizes.into_any_element()).child(small_note(&format!(
            "Recorded at {recorded}p from a {native}p game. Upscaling sharpens a bigger picture; it can't bring back detail the recording didn't keep."
        )));
        if picked.is_some() && self.ed.export_settings.max_bytes.is_some() {
            column = column.child(small_note("With a size limit, the upscaled size stays and the quality goes down to fit."));
        }
        if let Some(notice) = self.upscale_notice_for(picked, cx) {
            column = column.child(div().text_size(px(11.)).line_height(px(15.)).text_color(c(theme::MUTED)).child(crate::trace::target("upscale-notice")).child(notice));
        }
        column.into_any_element()
    }

    /// The upscaler notice for the picked upscale, asking the engine once per clip and size
    /// (it checks on the GPU). None until it answers, and when the AI upscaler runs.
    fn upscale_notice_for(&mut self, picked: Option<u32>, cx: &mut Context<Self>) -> Option<String> {
        let info = self.ed.info.as_ref()?;
        let key: crate::root::UpscaleKey = (info.path.clone(), (info.width, info.height), self.ed.upscaled_size(picked?)?);
        if let Some((known, notice)) = &self.upscale_notice {
            if *known == key {
                return notice.clone();
            }
        }
        if crate::TESTING || self.upscale_asked.as_ref() == Some(&key) {
            return None;
        }
        self.upscale_asked = Some(key.clone());
        cx.spawn(async move |this, cx| {
            let (input, output) = (key.1, key.2);
            let answer = cx.background_executor().spawn(async move { crate::backend::upscale_notice(input, output) }).await;
            let _ = this.update(cx, |this, cx| {
                // A failed check shows nothing: the export still picks an upscaler itself.
                this.upscale_notice = Some((key, answer.ok().flatten()));
                this.upscale_asked = None;
                cx.notify();
            });
        })
        .detach();
        None
    }

    fn render_custom_fps(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let custom = self.fps_custom;
        let recorded = self.ed.fps_ceiling();
        let toggle = ui::switch("fps-custom", custom).child(crate::trace::target("fps-custom")).on_click(cx.listener(|this, _, window, cx| {
            if this.fps_custom {
                // Back to the nearest offered rate at or below the typed one.
                let fps = this.ed.fps;
                let preset = this.ed.fps_choices().into_iter().filter(|p| *p <= fps).max().unwrap_or(fps);
                this.fps_custom = false;
                this.ed.set_fps(preset);
                window.focus(&this.focus);
            } else {
                this.fps_custom = true;
                // Typing a rate is choosing it yourself: an automatic fit lets go.
                this.ed.set_export_priority(ExportPriority::Manual);
                this.fps_field.set(this.ed.fps.to_string());
                window.focus(&this.fps_field.focus);
            }
            cx.notify();
        }));
        let mut column = div().flex().flex_col().gap(px(6.)).child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().flex_1().text_size(px(12.)).text_color(c(theme::MUTED)).child("Custom frame rate"))
                .child(toggle),
        );
        if custom {
            column = column.child(
                div()
                    .child(crate::trace::target("fps-field"))
                    .h(px(30.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(6.))
                    .bg(c(theme::RAISED))
                    .border_1()
                    .border_color(c(if self.fps_field.is_focused(window) { theme::BORDER_STRONG } else { theme::BORDER }))
                    .child(self.fps_field.render("fps-input", window).flex_1().on_click(cx.listener(|this, _, window, cx| {
                        window.focus(&this.fps_field.focus);
                        cx.notify();
                    })))
                    .child(div().text_color(c(theme::TERTIARY)).child("fps")),
            );
        }
        column.child(small_note(&format!("Any whole rate up to {recorded} fps, the clip's recorded rate."))).into_any_element()
    }
}

fn labelled(label: &str, control: AnyElement) -> gpui::Div {
    div().flex().flex_col().gap(px(6.)).child(div().text_size(px(12.)).text_color(c(theme::MUTED)).child(label.to_owned())).child(control)
}

fn small_note(text: &str) -> gpui::Div {
    div().text_size(px(11.)).line_height(px(15.)).text_color(c(theme::TERTIARY)).child(text.to_owned())
}
