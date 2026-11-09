//! Library page: sidebar + day-grouped, virtualized thumbnail grid.
//! Header, sort menu and selection bar here; the grid and tiles in `library_grid.rs`,
//! the sidebar in `library_sidebar.rs`.
use gpui::{AnyElement, Context, FontWeight, Window, deferred, div, prelude::*, px};

use crate::{
    root::{Root, Route},
    services::library::SortOrder,
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    worker::Job,
};

const GAP: f32 = 16.0;
const CONTENT_PAD: f32 = 24.0;

#[path = "library_grid.rs"]
mod grid;
#[path = "library_sidebar.rs"]
mod sidebar;
#[path = "library_damage.rs"]
mod damage;

impl Root {
    /// Fixed frame for every non-editor page: the same sidebar and the same panel,
    /// so navigation never moves; only the panel content changes.
    pub fn render_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.setup.is_some() {
            return self.render_setup(window, cx);
        }
        let content = match self.route {
            Route::Hdr => self.render_hdr(window, cx),
            Route::Audio => self.render_audio(window, cx),
            Route::Settings => self.render_settings(window, cx),
            _ => div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.render_library_header(window, cx))
                .children(self.render_library_damage(cx))
                .child(self.render_library_body(cx))
                .into_any_element(),
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .pr(px(8.))
            .pb(px(8.))
            .child(self.render_sidebar(cx))
            .child(ui::panel().flex_1().min_w_0().flex().flex_col().child(content))
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
            .child(div().font_family(theme::display()).text_size(px(22.)).line_height(px(28.)).font_weight(FontWeight::BOLD).child(title))
            .child(div().pt(px(2.)).text_size(px(14.)).text_color(c(theme::TERTIARY)).child(thousands(self.lib.visible_count)))
            .child(div().flex_1())
            .children(self.render_resume(cx))
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
                                this.sort_open = !this.press_saw_sort;
                                cx.notify();
                            })),
                    )
                    .when(self.sort_open, |d| d.child(self.render_sort_menu(cx))),
            )
            .into_any_element()
    }

    /// "Resume <title>": reopens the clip played last.
    fn render_resume(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let record = self.lib.last_played()?;
        let path = record.path.clone();
        let button = ui::button("resume", Variant::Ghost)
            .border_1()
            .border_color(c(theme::BORDER))
            .child(icon(Icon::Play, 14., theme::TEXT))
            .child("Resume")
            .child(div().max_w(px(200.)).min_w_0().truncate().text_color(c(theme::MUTED)).child(record.display_title().to_owned()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_clip(path.clone());
                cx.notify();
            }));
        Some(button.into_any_element())
    }

    fn render_sort_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.lib.sort;
        let panel = div()
            .id("sort-menu")
            .occlude()
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
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.sort_open = false;
                cx.notify();
            }))
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
            }));
        // Deferred like the tile menu: the grid paints after the header and would cover it.
        deferred(panel).with_priority(1).into_any_element()
    }

    fn render_selection_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let count = self.lib.selection.len();
        let bar =div().flex_none().flex().items_center().gap(px(8.)).px(px(CONTENT_PAD)).pt(px(20.)).pb(px(16.));
        if self.lib.deleting > 0 {
            let moving = self.lib.deleting;
            let noun = if moving == 1 { "clip" } else { "clips" };
            // A delete of a clip whose edits are still being saved waits for that save first.
            let text = if self.lib.delete_waiting { "Waiting for the edit save…".to_owned() } else { format!("Moving {moving} {noun} to the Recycle Bin…") };
            return bar.child(icon(Icon::Trash, 16., theme::MUTED)).child(div().text_color(c(theme::MUTED)).child(text)).into_any_element();
        }
        if self.lib.confirm_delete {
            let (question, detail, action) = delete_wording(count, self.imported_in_selection());
            return bar
                .child(icon(Icon::Trash, 16., theme::ERROR))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(question))
                .child(div().text_color(c(theme::TERTIARY)).child(detail))
                .child(div().flex_1())
                .child(ui::button("delete-cancel", Variant::Ghost).child("Keep").on_click(cx.listener(|this, _, _, cx| {
                    this.lib.confirm_delete = false;
                    cx.notify();
                })))
                .child(ui::button("delete-confirm", Variant::Primary).child(action).on_click(cx.listener(|this, _, _, cx| {
                    let paths = this.lib.selected_paths();
                    this.lib.confirm_delete = false;
                    this.lib.delete_started(paths.len());
                    // The folder list the question above was worded from decides owned vs imported.
                    this.workers.job(Job::Delete(paths, this.folders.clone()));
                    cx.notify();
                })))
                .into_any_element();
        }
        bar.child(div().font_family(theme::display()).text_size(px(22.)).line_height(px(28.)).font_weight(FontWeight::BOLD).child(format!("{count} selected")))
            .child(div().flex_1())
            .when(count == 1, |bar| {
                bar.child(ui::button("sel-open", Variant::Primary).child(icon(Icon::Play, 15., theme::BASE)).child("Open").child(ui::kbd("Enter")).on_click(cx.listener(|this, _, _, cx| {
                    if let Some(path) = this.lib.only_selected() {
                        this.open_clip(path);
                        this.opened_by_click = true;
                    }
                    cx.notify();
                })))
            })
            .child(ui::button("sel-fav", Variant::Secondary).child(icon(Icon::Star, 15., theme::TEXT)).child("Favourite").on_click(cx.listener(|this, _, _, cx| {
                let paths: Vec<String> = this.lib.selected_paths().iter().map(|p| p.to_string_lossy().into_owned()).collect();
                this.lib.toggle_favourite(&paths);
                cx.notify();
            })))
            .child(ui::button("sel-copy", Variant::Secondary).child(icon(Icon::Copy, 15., theme::TEXT)).child("Copy").on_click(cx.listener(|this, _, _, cx| {
                let paths: Vec<String> = this.lib.selected_paths().iter().map(|p| p.to_string_lossy().into_owned()).collect();
                this.copy_clips(paths);
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
        .child(crate::trace::target(id))
        .text_color(c(if active { theme::TEXT } else { theme::MUTED }))
        .when(active, |d| d.bg(c(theme::OVERLAY)))
        .when(!active, |d| d.hover(|s| s.bg(c(theme::RAISED)).text_color(c(theme::TEXT))))
        .child(icon(kind, 16., if active { theme::TEXT } else { theme::MUTED }))
        .child(div().flex_1().font_weight(if active { FontWeight::MEDIUM } else { FontWeight::NORMAL }).child(label.to_owned()))
        .children(count.map(|n| div().text_size(px(12.)).text_color(c(theme::TERTIARY)).child(n)))
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
        .child(div().font_family(theme::display()).text_size(px(17.)).font_weight(FontWeight::BOLD).child(title.to_owned()))
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

impl Root {
    /// Puts clips on the clipboard as files (paste into Discord, Explorer, chats).
    pub fn copy_clips(&mut self, paths: Vec<String>) {
        let count = paths.len();
        match crate::shell::copy_files(&paths) {
            Ok(()) => {
                let what = if count == 1 { "Clip copied".to_owned() } else { format!("{count} clips copied") };
                self.show_toast(crate::root::ToastKind::Good, what, Some("Paste it into Discord or a folder".into()));
            }
            Err(error) => self.show_toast(crate::root::ToastKind::Bad, "Couldn't copy", Some(error)),
        }
    }

    /// Drags a clip (or the whole selection when the tile is selected) out of Recall.
    /// The OLE modal loop runs in a spawned task, outside any GPUI state borrow.
    pub fn start_drag(&mut self, path: String, cx: &mut Context<Self>) {
        let paths: Vec<String> = if self.lib.selection.contains(&path) {
            self.lib.selected_paths().iter().map(|p| p.to_string_lossy().into_owned()).collect()
        } else {
            vec![path]
        };
        self.drag_paths(paths, cx);
    }

    /// Runs an OLE file drag of `paths` (a library selection or a finished export).
    pub fn drag_paths(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = crate::shell::drag_files(&paths);
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.show_toast(crate::root::ToastKind::Bad, "Couldn't drag", Some(error));
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Root {
    /// Selected clips that live in an imported folder (Delete only removes them from Recall).
    fn imported_in_selection(&self) -> usize {
        let recordings = crate::backend::library_dir_from(&self.folders);
        self.lib.selection.iter().filter(|p| !crate::services::library::imports::is_owned(
            std::path::Path::new(p), &recordings, &self.folders.former_recordings
        )).count()
    }
}

/// The delete question, its detail line and the button, for `count` clips of which `imported`
/// are in folders another recorder owns.
fn delete_wording(count: usize, imported: usize) -> (String, String, &'static str) {
    let clips = |n: usize| format!("{n} {}", if n == 1 { "clip" } else { "clips" });
    if imported == 0 {
        (format!("Move {} to the Recycle Bin?", clips(count)), "Thumbnails and edits go with them.".into(), "Move to Recycle Bin")
    } else if imported == count {
        (format!("Remove {} from Recall?", clips(count)), "The files stay in their folder.".into(), "Remove from Recall")
    } else {
        (format!("Remove {}?", clips(count)), format!("{} go to the Recycle Bin. {imported} from other folders stay where they are.", count - imported), "Remove")
    }
}
