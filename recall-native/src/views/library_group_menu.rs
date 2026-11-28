//! The library header's "Group by" menu: "No groups", then a Date column (time window, day,
//! week, month, year) and a Game column, so the two can be set together.
use gpui::{AnyElement, Context, deferred, div, prelude::*, px};

use super::{menu_item, menu_panel};
use crate::{
    root::Root,
    services::library::{DateGroup, GroupBy},
    theme::{self, c},
};

impl Root {
    /// "Group by": "No groups", then one column per grouping, each listing its spans. A pick in a
    /// column keeps the menu open, so a date span and games can be set together. Regroups the
    /// clips already listed; nothing is read from disk.
    pub(super) fn render_group_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.lib.group;
        let date_column = column("Date").children(DateGroup::ALL.into_iter().enumerate().map(|(n, date)| {
            menu_item(("group-date", n), date_label(date), current.date == date)
                .child(crate::trace::target(format!("group-date-{}", date_word(date))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let group = GroupBy { date, ..this.lib.group };
                    this.lib.set_group(group);
                    cx.notify();
                }))
        }));
        let game_column = column("Game").children([false, true].into_iter().map(|game| {
            menu_item(("group-game", game as usize), if game { "By game" } else { "Off" }, current.game == game)
                .child(crate::trace::target(format!("group-game-{}", if game { "on" } else { "off" })))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let group = GroupBy { game, ..this.lib.group };
                    this.lib.set_group(group);
                    cx.notify();
                }))
        }));
        let panel = menu_panel("group-menu", cx)
            .w(px(GROUP_MENU_WIDTH))
            .child(div().h(px(26.)).px(px(10.)).flex().items_center().text_size(px(11.)).text_color(c(theme::TERTIARY)).child("Group by"))
            .child(
                menu_item(("group-none", 0), "No groups", current.is_none()).child(crate::trace::target("group-none")).on_click(cx.listener(|this, _, _, cx| {
                    this.sort_open = false;
                    this.lib.set_group(GroupBy::None);
                    cx.notify();
                })),
            )
            .child(div().my(px(4.)).h(px(1.)).bg(c(theme::BORDER)))
            .child(div().flex().gap(px(4.)).child(date_column).child(game_column));
        deferred(panel).with_priority(1).into_any_element()
    }
}

/// Room for the Date and Game columns side by side.
const GROUP_MENU_WIDTH: f32 = 300.0;

/// A Group by column: its heading and, below, its choices.
fn column(heading: &'static str) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(div().h(px(24.)).px(px(10.)).flex().items_center().text_size(px(11.)).text_color(c(theme::TERTIARY)).child(heading))
}

/// The Date column's choices.
fn date_label(date: DateGroup) -> &'static str {
    match date {
        DateGroup::Off => "Off",
        DateGroup::Time => "Time window",
        DateGroup::Day => "Day",
        DateGroup::Week => "Week",
        DateGroup::Month => "Month",
        DateGroup::Year => "Year",
    }
}

fn date_word(date: DateGroup) -> &'static str {
    match date {
        DateGroup::Off => "off",
        DateGroup::Time => "time",
        DateGroup::Day => "day",
        DateGroup::Week => "week",
        DateGroup::Month => "month",
        DateGroup::Year => "year",
    }
}

/// The Group by button: "By day" or "By day and game" in a wide header, "Day" in a narrow one
/// (the game then shows as an icon). "No groups" stays at every width: "None" alone would not
/// say what it is.
pub(super) fn group_button(group: GroupBy, long: bool) -> &'static str {
    let span = match group.date {
        DateGroup::Off => return if group.game { if long { "By game" } else { "Game" } } else { "No groups" },
        DateGroup::Time => ("Time", "By time", "By time and game"),
        DateGroup::Day => ("Day", "By day", "By day and game"),
        DateGroup::Week => ("Week", "By week", "By week and game"),
        DateGroup::Month => ("Month", "By month", "By month and game"),
        DateGroup::Year => ("Year", "By year", "By year and game"),
    };
    match (long, group.game) {
        (false, _) => span.0,
        (true, false) => span.1,
        (true, true) => span.2,
    }
}
