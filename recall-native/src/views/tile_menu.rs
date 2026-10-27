//! Right-click menu on library clip tiles: Open, Favourite, Rename, Copy, Show in folder and
//! Delete. Every item runs the action behind the tile's hover buttons or the selection bar;
//! Delete hands over to the selection bar's inline "Move to Recycle Bin?" confirm.
use gpui::{AnyElement, Context, Pixels, Point, Window, anchored, deferred, div, prelude::*, px};

use crate::{
    root::Root,
    theme::{self, c, ca},
    ui::{self, Icon, icon},
};

/// An open tile menu.
pub struct TileMenu {
    /// The clip that was right-clicked (Open, Rename and Show in folder act on it).
    pub path: String,
    /// What Favourite, Copy and Delete act on: the selection when the clip is part of it.
    pub targets: Vec<String>,
    /// Pointer position in window coordinates.
    pub at: Point<Pixels>,
}

#[derive(Clone, Copy)]
enum Action {
    Open,
    Favourite,
    Rename,
    Copy,
    ShowInFolder,
    Delete,
}

impl Root {
    pub fn open_tile_menu(&mut self, path: String, at: Point<Pixels>) {
        let targets = self.lib.menu_targets(&path);
        self.lib.confirm_delete = false;
        self.sort_open = false;
        self.tile_menu = Some(TileMenu { path, targets, at });
    }

    /// Closes the menu; true when one was open.
    pub fn close_tile_menu(&mut self) -> bool {
        self.tile_menu.take().is_some()
    }

    fn run_tile_action(&mut self, action: Action, window: &mut Window) {
        let Some(menu) = self.tile_menu.take() else { return };
        match action {
            Action::Open => self.open_clip(menu.path),
            Action::Favourite => self.lib.toggle_favourite(&menu.targets),
            Action::Rename => self.begin_rename(menu.path, window),
            Action::Copy => self.copy_clips(menu.targets),
            Action::ShowInFolder => crate::backend::show_in_folder(&menu.path),
            Action::Delete => self.lib.ask_delete(&menu.targets),
        }
    }

    pub(super) fn render_tile_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.tile_menu.as_ref()?;
        let count = menu.targets.len();
        let all_favourite = menu.targets.iter().all(|p| self.lib.record(p).is_some_and(|r| r.favourite));
        let many = |one: &str, several: &str| if count > 1 { format!("{several} {count} clips") } else { one.to_owned() };
        let favourite = if all_favourite {
            (Icon::StarFill, many("Remove from favourites", "Unfavourite"))
        } else {
            (Icon::Star, many("Add to favourites", "Favourite"))
        };
        let copy = if count > 1 { format!("Copy {count} files") } else { "Copy file".to_owned() };
        let item = |id: &'static str, kind: Icon, label: String, action: Action, cx: &mut Context<Self>| {
            let danger = matches!(action, Action::Delete);
            let tint = if danger { theme::ERROR } else { theme::TEXT };
            div()
                .id(id)
                .h(px(30.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(5.))
                .cursor_pointer()
                .text_color(c(tint))
                .hover(|s| s.bg(c(theme::TOP)))
                .child(icon(kind, 15., if danger { theme::ERROR } else { theme::MUTED }))
                .child(div().flex_1().child(label))
                .when(danger, |d| d.child(ui::kbd("Del")))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.run_tile_action(action, window);
                    cx.stop_propagation();
                    cx.notify();
                }))
        };
        let panel = div()
            .id("tile-menu")
            .occlude()
            .w(px(228.))
            .p(px(4.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .bg(c(theme::FLOATING))
            .border_1()
            .border_color(c(theme::BORDER))
            .shadow(vec![gpui::BoxShadow {
                color: ca(0x000000, 0.5),
                offset: gpui::point(px(0.), px(8.)),
                blur_radius: px(24.),
                spread_radius: px(0.),
            }])
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.close_tile_menu() {
                    cx.notify();
                }
            }))
            .child(item("tile-menu-open", Icon::Play, "Open".into(), Action::Open, cx))
            .child(item("tile-menu-fav", favourite.0, favourite.1, Action::Favourite, cx))
            .child(item("tile-menu-rename", Icon::Pencil, "Rename".into(), Action::Rename, cx))
            .child(item("tile-menu-copy", Icon::Copy, copy, Action::Copy, cx))
            .child(item("tile-menu-folder", Icon::Folder, "Show in folder".into(), Action::ShowInFolder, cx))
            .child(div().my(px(4.)).mx(px(6.)).h(px(1.)).bg(c(theme::BORDER)))
            .child(item("tile-menu-delete", Icon::Trash, many("Delete", "Delete"), Action::Delete, cx));
        Some(
            deferred(anchored().position(menu.at).snap_to_window_with_margin(px(8.)).child(panel))
                .with_priority(1)
                .into_any_element(),
        )
    }
}
