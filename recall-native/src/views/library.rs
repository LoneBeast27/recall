//! Library page: sidebar + day-grouped, virtualized thumbnail grid.
use std::path::PathBuf;

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, MouseMoveEvent, ObjectFit, Window, canvas, div, img, list,
    prelude::*, px,
};

use crate::{
    library_state::Row,
    root::{Root, Route},
    services::library::{self, SortOrder},
    theme::{self, c, ca},
    ui::{self, Icon, Variant, icon},
    worker::Job,
};

const GAP: f32 = 16.0;
const CONTENT_PAD: f32 = 24.0;

impl Root {
    pub fn render_library_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .pr(px(8.))
            .pb(px(8.))
            .child(self.render_sidebar(cx))
            .child(
                ui::panel()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(self.render_library_header(window, cx))
                    .child(self.render_library_body(cx)),
            )
            .into_any_element()
    }

    fn render_library_header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.lib.selection.is_empty() {
            return self.render_selection_bar(cx);
        }
        let title = match self.lib.scope() {
            crate::library_state::Scope::All => "All clips".to_owned(),
            crate::library_state::Scope::Favourites => "Favourites".to_owned(),
            crate::library_state::Scope::Game => self
                .lib
                .games
                .iter()
                .find(|g| Some(&g.key) == self.lib.filter.game_key.as_ref())
                .map_or_else(|| "Game".into(), |g| g.name.clone()),
        };
        let sort_label = sort_label(self.lib.sort);
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(CONTENT_PAD))
            .pt(px(20.))
            .pb(px(16.))
            .child(div().text_size(px(20.)).line_height(px(28.)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().pt(px(2.)).text_size(px(14.)).text_color(c(theme::TERTIARY)).child(thousands(self.lib.visible_count)))
            .child(div().flex_1())
            .child(
                div()
                    .w(px(260.))
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pl(px(10.))
                    .pr(px(6.))
                    .rounded(px(6.))
                    .bg(c(theme::RAISED))
                    .border_1()
                    .border_color(c(if self.search.is_focused(window) { theme::BORDER_STRONG } else { theme::BORDER }))
                    .child(icon(Icon::Search, 15., theme::PLACEHOLDER))
                    .child(
                        self.search
                            .render("search", window)
                            .flex_1()
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.focus(&this.search.focus);
                                cx.notify();
                            })),
                    )
                    .child(ui::kbd("Ctrl K")),
            )
            .child(
                div()
                    .relative()
                    .child(
                        ui::button("sort", Variant::Ghost)
                            .border_1()
                            .border_color(c(theme::BORDER))
                            .child(sort_label)
                            .child(icon(Icon::ChevronDown, 14., theme::MUTED))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sort_open = !this.sort_open;
                                cx.notify();
                            })),
                    )
                    .when(self.sort_open, |d| d.child(self.render_sort_menu(cx))),
            )
            .into_any_element()
    }

    fn render_sort_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.lib.sort;
        div()
            .absolute()
            .top(px(38.))
            .right_0()
            .w(px(180.))
            .p(px(4.))
            .rounded(px(8.))
            .bg(c(theme::FLOATING))
            .border_1()
            .border_color(c(theme::BORDER))
            .flex()
            .flex_col()
            .children([SortOrder::Newest, SortOrder::Oldest, SortOrder::Longest, SortOrder::Largest].into_iter().map(|order| {
                div()
                    .id(("sort-item", order as usize))
                    .h(px(30.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(theme::TOP)))
                    .child(sort_label(order))
                    .when(order == current, |d| d.child(icon(Icon::Check, 14., theme::TEXT)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.lib.sort = order;
                        this.sort_open = false;
                        this.lib.refilter();
                        cx.notify();
                    }))
            }))
            .into_any_element()
    }

    fn render_selection_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.lib.selection.len();
        let noun = if count == 1 { "clip" } else { "clips" };
        let bar = div().flex_none().flex().items_center().gap(px(8.)).px(px(CONTENT_PAD)).pt(px(20.)).pb(px(16.));
        if self.lib.confirm_delete {
            return bar
                .child(icon(Icon::Trash, 16., theme::ERROR))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Move {count} {noun} to the Recycle Bin?")))
                .child(div().text_color(c(theme::TERTIARY)).child("Thumbnails and edits go with them."))
                .child(div().flex_1())
                .child(ui::button("delete-cancel", Variant::Ghost).child("Keep").on_click(cx.listener(|this, _, _, cx| {
                    this.lib.confirm_delete = false;
                    cx.notify();
                })))
                .child(ui::button("delete-confirm", Variant::Primary).child("Move to Recycle Bin").on_click(cx.listener(|this, _, _, cx| {
                    let paths = this.lib.selected_paths();
                    this.lib.confirm_delete = false;
                    this.workers.job(Job::Delete(paths));
                    cx.notify();
                })))
                .into_any_element();
        }
        bar.child(div().text_size(px(20.)).line_height(px(28.)).font_weight(FontWeight::SEMIBOLD).child(format!("{count} selected")))
            .child(div().flex_1())
            .child(ui::button("sel-fav", Variant::Secondary).child(icon(Icon::Star, 15., theme::TEXT)).child("Favourite").on_click(cx.listener(|this, _, _, cx| {
                let paths: Vec<String> = this.lib.selected_paths().iter().map(|p| p.to_string_lossy().into_owned()).collect();
                this.lib.toggle_favourite(&paths);
                cx.notify();
            })))
            .child(ui::button("sel-delete", Variant::Secondary).child(icon(Icon::Trash, 15., theme::TEXT)).child("Delete").on_click(cx.listener(|this, _, _, cx| {
                this.lib.confirm_delete = true;
                cx.notify();
            })))
            .child(ui::button("sel-clear", Variant::Ghost).child("Cancel").child(ui::kbd("Esc")).on_click(cx.listener(|this, _, _, cx| {
                this.lib.clear_selection();
                cx.notify();
            })))
            .into_any_element()
    }

    fn render_library_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
                    .child(div().text_size(px(14.)).line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).child(g.label.clone()))
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
                .items_center()
                .justify_center()
                .child(icon(Icon::Film, 20., theme::DISABLED))
                .into_any_element(),
        };
        let badge = match (hovered, record.duration) {
            (true, Some(d)) => Some(format!("{} / {}", library::format_duration(d * self.lib.hover_frac as f64), library::format_duration(d))),
            (_, Some(d)) => Some(library::format_duration(d)),
            _ => None,
        };
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
            .when(record.favourite && !hovered, |d| {
                d.child(div().absolute().top(px(8.)).left(px(8.)).child(icon(Icon::StarFill, 14., theme::TEXT)))
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
                    .font_family(theme::MONO)
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .child(b)
            }));
        let title_row: AnyElement = if renaming {
            div()
                .h(px(26.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(5.))
                .bg(c(theme::RAISED))
                .border_1()
                .border_color(c(theme::BORDER_STRONG))
                .child(self.rename.render("rename", window).flex_1())
                .into_any_element()
        } else {
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap(px(8.))
                .child(div().font_weight(FontWeight::MEDIUM).min_w_0().truncate().child(record.display_title().to_owned()))
                .child(div().font_family(theme::MONO).text_size(px(11.)).text_color(c(theme::TERTIARY)).child(time))
                .into_any_element()
        };
        let open_path = path.clone();
        let hover_path = path.clone();
        div()
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
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if let Some((left, width)) = this.lib.hover_span.get() {
                    let frac = ((f32::from(event.position.x) - left) / width.max(1.0)).clamp(0.0, 0.999);
                    if (frac - this.lib.hover_frac).abs() > 0.004 {
                        this.lib.hover_frac = frac;
                        cx.notify();
                    }
                }
            }))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                let modifiers = event.modifiers();
                if event.click_count() == 2 && !modifiers.control && !modifiers.shift {
                    this.begin_rename(open_path.clone(), window);
                } else if this.lib.click(&open_path, modifiers.control, modifiers.shift) {
                    this.open_clip(open_path.clone());
                }
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
        let fav_path = path.to_owned();
        let folder_path = path.to_owned();
        let rename_path = path.to_owned();
        div()
            .absolute()
            .top(px(6.))
            .right(px(6.))
            .flex()
            .gap(px(4.))
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
            .child(tile_action("tile-folder", Icon::Folder).on_click(cx.listener(move |_, _, _, cx| {
                crate::backend::show_in_folder(&folder_path);
                cx.stop_propagation();
            })))
            .into_any_element()
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.lib.scope();
        let games_shown = if self.lib.show_all_games { self.lib.games.len() } else { self.lib.games.len().min(10) };
        let games: Vec<_> = self.lib.games.iter().take(games_shown).cloned().collect();
        let selected_game = self.lib.filter.game_key.clone();
        div()
            .w(px(232.))
            .flex_none()
            .flex()
            .flex_col()
            .px(px(12.))
            .pt(px(4.))
            .pb(px(4.))
            .gap(px(2.))
            .child(nav_row("nav-all", Icon::Grid, "All clips", Some(thousands(self.lib.records.len())), scope == crate::library_state::Scope::All).on_click(
                cx.listener(|this, _, _, cx| {
                    this.lib.filter.game_key = None;
                    this.lib.filter.favourites_only = false;
                    this.lib.refilter();
                    cx.notify();
                }),
            ))
            .child(nav_row("nav-fav", Icon::Star, "Favourites", Some(thousands(self.lib.favourites)), scope == crate::library_state::Scope::Favourites).on_click(
                cx.listener(|this, _, _, cx| {
                    this.lib.filter.game_key = None;
                    this.lib.filter.favourites_only = true;
                    this.lib.refilter();
                    cx.notify();
                }),
            ))
            .child(div().pt(px(18.)).pb(px(6.)).px(px(10.)).child(ui::caption("Games")))
            .child(
                div()
                    .id("games")
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .flex_shrink()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(games.into_iter().map(|game| {
                        let key = game.key.clone();
                        let active = selected_game.as_deref() == Some(game.key.as_str());
                        game_row(&game.key, &game.name, game.count, active).on_click(cx.listener(move |this, _, _, cx| {
                            this.lib.filter.favourites_only = false;
                            this.lib.filter.game_key = Some(key.clone());
                            this.lib.refilter();
                            cx.notify();
                        }))
                    }))
                    .when(self.lib.games.len() > 10, |d| {
                        let label = if self.lib.show_all_games { "Show fewer" } else { "Show all games" };
                        d.child(
                            div()
                                .id("games-more")
                                .h(px(30.))
                                .px(px(10.))
                                .flex()
                                .items_center()
                                .text_size(px(12.))
                                .text_color(c(theme::TERTIARY))
                                .cursor_pointer()
                                .hover(|s| s.text_color(c(theme::TEXT)))
                                .child(format!("{label} ({})", self.lib.games.len()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.lib.show_all_games = !this.lib.show_all_games;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(div().flex_1())
            .child(div().mx(px(-12.)).child(ui::hairline()))
            .child(div().pt(px(8.)))
            .child(nav_row("nav-hdr", Icon::Sun, "Display & HDR", None, false).on_click(cx.listener(|this, _, _, cx| {
                this.go(Route::Hdr);
                cx.notify();
            })))
            .child(nav_row("nav-audio", Icon::AudioLines, "Audio", None, false).on_click(cx.listener(|this, _, _, cx| {
                this.go(Route::Audio);
                cx.notify();
            })))
            .child(nav_row("nav-settings", Icon::Settings, "Settings", None, false).on_click(cx.listener(|this, _, _, cx| {
                this.go(Route::Settings);
                cx.notify();
            })))
            .children(self.lib.drive.map(|(free, total)| drive_meter(free, total)))
            .into_any_element()
    }
}

pub fn nav_row(id: &'static str, kind: Icon, label: &str, count: Option<String>, active: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(32.))
        .px(px(10.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_color(c(if active { theme::TEXT } else { theme::MUTED }))
        .when(active, |d| d.bg(c(theme::OVERLAY)))
        .when(!active, |d| d.hover(|s| s.bg(c(theme::RAISED)).text_color(c(theme::TEXT))))
        .child(icon(kind, 16., if active { theme::TEXT } else { theme::MUTED }))
        .child(div().flex_1().font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL }).child(label.to_owned()))
        .children(count.map(|n| div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(n)))
}

fn game_row(key: &str, name: &str, count: usize, active: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(gpui::SharedString::from(format!("game-{key}")))
        .h(px(30.))
        .px(px(10.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_color(c(if active { theme::TEXT } else { theme::MUTED }))
        .when(active, |d| d.bg(c(theme::OVERLAY)))
        .when(!active, |d| d.hover(|s| s.bg(c(theme::RAISED)).text_color(c(theme::TEXT))))
        .child(div().flex_1().min_w_0().truncate().child(name.to_owned()))
        .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(thousands(count)))
}

fn tile_action(id: &'static str, kind: Icon) -> gpui::Stateful<gpui::Div> {
    div()
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

fn drive_meter(free: u64, total: u64) -> AnyElement {
    let used = if total > 0 { 1.0 - free as f32 / total as f32 } else { 0.0 };
    div()
        .pt(px(10.))
        .px(px(10.))
        .pb(px(6.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .text_color(c(theme::MUTED))
                .child(icon(Icon::Drive, 16., theme::MUTED))
                .child(div().flex_1().child("Clip drive"))
                .child(div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(format!("{} free", library::format_bytes(free)))),
        )
        .child(
            div().ml(px(26.)).h(px(3.)).rounded(px(2.)).bg(c(theme::BORDER_SUBTLE)).child(
                div().h_full().rounded(px(2.)).w(gpui::relative(used.clamp(0.0, 1.0))).bg(c(if used > 0.9 { theme::WARNING } else { theme::DISABLED })),
            ),
        )
        .into_any_element()
}

pub fn empty_state(kind: Icon, title: &str, detail: Option<String>) -> AnyElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.))
        .child(icon(kind, 28., theme::DISABLED))
        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(title.to_owned()))
        .children(detail.map(|d| div().max_w(px(420.)).text_color(c(theme::TERTIARY)).text_center().child(d)))
        .into_any_element()
}

fn sort_label(order: SortOrder) -> &'static str {
    match order {
        SortOrder::Newest => "Newest first",
        SortOrder::Oldest => "Oldest first",
        SortOrder::Longest => "Longest first",
        SortOrder::Largest => "Largest first",
    }
}

pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
