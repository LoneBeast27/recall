//! Library sidebar: scopes, the games list, page links and the clip-drive meter.
use gpui::{AnyElement, Context, div, prelude::*, px};

use super::{nav_row, thousands};
use crate::{
    root::{Root, Route},
    services::library,
    theme::{self, c},
    ui::{self, Icon, icon},
};

impl Root {
    pub(super) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let on_library = self.route == Route::Library;
        let scope = if on_library { Some(self.lib.scope()) } else { None };
        let route = self.route;
        let games_shown = if self.lib.show_all_games { self.lib.games.len() } else { self.lib.games.len().min(10) };
        let games: Vec<_> = self.lib.games.iter().take(games_shown).cloned().collect();
        let selected_game = if on_library { self.lib.filter.game_key.clone() } else { None };
        div()
            .w(px(232.))
            .flex_none()
            .flex()
            .flex_col()
            .px(px(12.))
            .pt(px(4.))
            .pb(px(4.))
            .gap(px(2.))
            .child(nav_row("nav-all", Icon::Grid, "All clips", Some(thousands(self.lib.records.len())), scope == Some(crate::library_state::Scope::All)).on_click(
                cx.listener(|this, _, _, cx| {
                    this.go(Route::Library);
                    this.lib.filter.game_key = None;
                    this.lib.filter.favourites_only = false;
                    this.lib.refilter();
                    cx.notify();
                }),
            ))
            .child(nav_row("nav-fav", Icon::Star, "Favourites", Some(thousands(self.lib.favourites)), scope == Some(crate::library_state::Scope::Favourites)).on_click(
                cx.listener(|this, _, _, cx| {
                    this.go(Route::Library);
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
                            this.go(Route::Library);
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
            .when(crate::root::hdr_page_enabled(), |d| {
                d.child(nav_row("nav-hdr", Icon::Sun, "Display & HDR", None, route == Route::Hdr).on_click(cx.listener(|this, _, _, cx| {
                    this.go(Route::Hdr);
                    cx.notify();
                })))
            })
            .child(nav_row("nav-audio", Icon::AudioLines, "Audio", None, route == Route::Audio).on_click(cx.listener(|this, _, _, cx| {
                this.go(Route::Audio);
                cx.notify();
            })))
            .child(nav_row("nav-settings", Icon::Settings, "Settings", None, route == Route::Settings).on_click(cx.listener(|this, _, _, cx| {
                this.go(Route::Settings);
                cx.notify();
            })))
            .children(self.lib.drive.map(|(free, total)| drive_meter(free, total)))
            .into_any_element()
    }
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
