//! Settings → Games: the apps registered by hand (the maintainer 2026-11-07: "use official proof +
//! support for manual application registering"). The recorder records an unknown fullscreen
//! app only with official proof; the island's "Not recording <exe>: add it as a game in
//! Settings" leads here. "Add app…" offers the running apps (the foreground one first, marked
//! "in front") or any exe on disk; each entry can be set to "Never record" or removed. Every
//! change is one `register_game` command (`services::game_apps`), applied by the controller
//! to its current config.
use gpui::{AnyElement, Context, PathPromptOptions, SharedString, Stateful, deferred, div, prelude::*, px};

use crate::{
    root::Root,
    services::{
        daemon::DaemonCommand,
        game_apps::{self, GameEntry, Pickable, Registration},
        game_apps_win,
    },
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    views::pages::{section_title, setting_row},
};

/// Settings → Games state for the launch.
#[derive(Default)]
pub struct GamesPage {
    /// "Add app…" is open.
    pub menu: bool,
    /// The running apps, once listed for this opening of the menu.
    pub apps: Option<Pickable>,
}

/// What the Games tab explains above the list.
pub(crate) const GAMES_INTRO: &str = "Recall records a game on its own when Steam, Epic, GOG, Ubisoft, EA, Battle.net, itch or the Xbox app installed it, when Recall or the NVIDIA driver knows it, or when it draws like a game. Add any other app here.";

fn panel(id: &'static str) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .occlude()
        .absolute()
        .top(px(36.))
        .right_0()
        .w(px(300.))
        .max_h(px(320.))
        .overflow_y_scroll()
        .p(px(4.))
        .rounded(px(8.))
        .bg(c(theme::FLOATING))
        .border_1()
        .border_color(c(theme::BORDER))
        .flex()
        .flex_col()
}

fn note(text: &str) -> AnyElement {
    div().px(px(10.)).py(px(6.)).text_size(px(11.)).text_color(c(theme::TERTIARY)).child(text.to_owned()).into_any_element()
}

impl Root {
    pub(crate) fn settings_games(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let config = self.snap.config.clone();
        let mut items = vec![section_title("Games"), div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(GAMES_INTRO).into_any_element()];
        let Some(config) = config else {
            items.push(note("Start the recorder to see and change this list."));
            return items;
        };
        let list = game_apps::entries(Some(&config));
        if list.is_empty() {
            items.push(note("No apps added yet."));
        }
        for (i, entry) in list.into_iter().enumerate() {
            items.push(self.game_row(i, entry, cx));
        }
        let mut add = div().relative().child(ui::button("games-add", Variant::Ghost).border_1().border_color(c(theme::BORDER)).child(icon(Icon::Plus, 14., theme::MUTED)).child("Add app…").on_click(cx.listener(|this, _, _, cx| {
            this.open_games_menu(cx);
            cx.notify();
        })));
        if self.settings_ui.games.menu {
            add = add.child(self.games_menu(cx));
        }
        items.push(div().flex().justify_end().child(add).into_any_element());
        items
    }

    fn game_row(&self, i: usize, entry: GameEntry, cx: &mut Context<Self>) -> AnyElement {
        let mut row = setting_row(&entry.exe, entry.how.label());
        if matches!(entry.how, Registration::Record | Registration::Never) {
            let mut track = ui::segment_track().w(px(220.));
            for (j, (how, label)) in [(Registration::Record, "Record"), (Registration::Never, "Never record")].into_iter().enumerate() {
                // The selected segment sends nothing (R8-detect-4).
                let command = game_apps::segment_command(&entry, how);
                track = track.child(ui::segment(("game-how", i * 2 + j), label, entry.how == how).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(command) = command.clone() {
                        this.send_games_command(command);
                    }
                    cx.notify();
                })));
            }
            row = row.child(track);
        }
        let remove = game_apps::remove_command(&entry);
        row.child(ui::button(("game-remove", i), Variant::Ghost).child("Remove").on_click(cx.listener(move |this, _, _, cx| {
            this.send_games_command(remove.clone());
            cx.notify();
        })))
        .into_any_element()
    }

    /// One app's change (`register_game`): the controller applies it to its current config.
    fn send_games_command(&mut self, command: DaemonCommand) {
        self.send(command);
        self.send(DaemonCommand::GetConfig);
    }

    /// Open "Add app…" and list the running apps off the UI thread.
    fn open_games_menu(&mut self, cx: &mut Context<Self>) {
        self.settings_ui.games = GamesPage { menu: true, apps: None };
        // The app in front is the foreground window now (R8-detect-3), not the first one found.
        let front = game_apps_win::foreground_pid();
        cx.spawn(async move |this, cx| {
            let apps = cx.background_executor().spawn(async move { game_apps_win::running_apps_blocking(front) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.settings_ui.games.menu {
                    this.settings_ui.games.apps = Some(apps);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn games_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        match self.settings_ui.games.apps.as_ref() {
            None => rows.push(note("Looking for running apps…")),
            Some(pick) if pick.apps.is_empty() => rows.push(note("No other app has a window open. Browse for its exe.")),
            Some(pick) => {
                for (i, app) in pick.apps.iter().enumerate() {
                    let label = if i == 0 && pick.in_front { format!("{} · in front", app.label) } else { app.label.clone() };
                    let command = game_apps::add_command(&app.exe);
                    rows.push(menu_row(("games-app", i), label, &app.exe).on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_ui.games = GamesPage::default();
                        this.send_games_command(command.clone());
                        cx.notify();
                    })).into_any_element());
                }
            }
        }
        rows.push(menu_row(("games-browse", 0), "Browse for an exe…".into(), "").on_click(cx.listener(|this, _, _, cx| {
            this.settings_ui.games = GamesPage::default();
            this.browse_for_game(cx);
            cx.notify();
        })).into_any_element());
        let panel = panel("games-menu").children(rows).on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.settings_ui.games = GamesPage::default();
            cx.notify();
        }));
        deferred(panel).with_priority(1).into_any_element()
    }

    /// Any exe on disk, registered as a game.
    fn browse_for_game(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(SharedString::from("Record this app as a game")) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(exe) = paths.into_iter().next().and_then(|p| exe_name(&p)) else { return };
            let _ = this.update(cx, |this, cx| {
                this.send_games_command(game_apps::add_command(&exe));
                cx.notify();
            });
        })
        .detach();
    }
}

/// The file name of a picked `.exe`, else `None`.
pub(crate) fn exe_name(path: &std::path::Path) -> Option<String> {
    path.extension().filter(|e| e.eq_ignore_ascii_case("exe"))?;
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

fn menu_row(id: (&'static str, usize), label: String, exe: &str) -> Stateful<gpui::Div> {
    let detail = (!exe.is_empty() && !label.eq_ignore_ascii_case(exe)).then(|| div().text_size(px(11.)).text_color(c(theme::TERTIARY)).child(exe.to_owned()));
    div()
        .id(id)
        .min_h(px(30.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(5.))
        .cursor_pointer()
        .hover(|s| s.bg(c(theme::TOP)))
        .child(icon(Icon::Gamepad, 14., theme::MUTED))
        .child(div().flex_1().min_w_0().truncate().child(label))
        .children(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_browsed_file_must_be_an_exe() {
        assert_eq!(exe_name(std::path::Path::new(r"X:\Games\Fixture\FixtureGame.EXE")).as_deref(), Some("FixtureGame.EXE"));
        assert_eq!(exe_name(std::path::Path::new(r"X:\Games\Fixture\readme.txt")), None);
        assert_eq!(exe_name(std::path::Path::new(r"X:\Games\Fixture")), None);
    }

    #[test]
    fn the_intro_names_every_official_proof() {
        for source in ["Steam", "Epic", "GOG", "Ubisoft", "EA", "Battle.net", "itch", "Xbox", "NVIDIA"] {
            assert!(GAMES_INTRO.contains(source), "{source}");
        }
    }
}
