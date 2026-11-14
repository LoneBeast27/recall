//! Settings → Fixes: problems Recall found that it can fix, each applied only
//! when the user clicks Fix. The (i) button lists every change exactly, from
//! the same plan the helper runs. Applied fixes keep an Undo. A failed fix
//! offers "Report this", which opens the report page filled in.

use std::collections::HashSet;
use std::sync::Mutex;

use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};

use crate::{
    root::{Root, Route},
    fix_offers::{self as fixes, Action, Last},
    theme::{self, c},
    ui::{self, Icon, Variant, icon},
    views::pages::{card, setting_row_with},
};
use recall_fixes::offer::Offer;
use recall_fixes::run::Result as RunResult;
use recall_fixes::{Plan, Receipt};

/// Which (i) lists are open, by fix and program (a view detail, not a setting).
static OPEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);
/// Applied fixes whose "fixed" line was closed this run (they show again at the next start).
static CLOSED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn receipt_key(r: &Receipt) -> String {
    key(&r.plan, &r.target)
}

/// The applied fixes the notice shows: every receipt on disk (also for a fix Recall no longer
/// offers, so it can still be undone after a restart), less those closed this run.
fn shown_receipts(applied: Vec<Receipt>, closed: &HashSet<String>) -> Vec<Receipt> {
    applied.into_iter().filter(|r| !closed.contains(&receipt_key(r))).collect()
}

fn closed_now() -> HashSet<String> {
    CLOSED.lock().unwrap_or_else(|p| p.into_inner()).clone().unwrap_or_default()
}

fn close_receipts(keys: impl IntoIterator<Item = String>) {
    CLOSED.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(HashSet::new).extend(keys);
}

fn key(plan: &Plan, target: &str) -> String {
    format!("{}|{}", plan.fix.as_str(), target.to_lowercase())
}

fn is_open(k: &str) -> bool {
    // Layout pictures of an offscreen app may show every (i) list open.
    if crate::offscreen::enabled() && std::env::var_os("RECALL_FIXES_PREVIEW_OPEN").is_some() {
        return true;
    }
    OPEN.lock().unwrap_or_else(|p| p.into_inner()).as_ref().is_some_and(|s| s.contains(k))
}

fn toggle(k: &str) {
    let mut open = OPEN.lock().unwrap_or_else(|p| p.into_inner());
    let set = open.get_or_insert_with(HashSet::new);
    if !set.remove(k) {
        set.insert(k.to_owned());
    }
}

/// The program's file name, for a short line ("javaw.exe").
fn short(target: &str) -> &str {
    target.rsplit('\\').next().unwrap_or(target)
}

/// The line under a row after a click on it.
pub(crate) fn result_line(last: &Last, plan: &Plan) -> (String, u32) {
    let after = plan.fix.after();
    match (&last.result, last.action) {
        (RunResult::Done, Action::Apply) => (after.map_or("Fixed.".to_owned(), |a| format!("Fixed. {a} for it to take effect.")), theme::MUTED),
        (RunResult::Done, Action::Undo) => (after.map_or("Undone.".to_owned(), |a| format!("Undone. {a} for it to take effect.")), theme::MUTED),
        (RunResult::DoneWithNote(n), _) => (n.clone(), theme::MUTED),
        (RunResult::Declined, _) => ("Nothing changed: the Windows prompt was declined.".to_owned(), theme::MUTED),
        (RunResult::Failed(e), _) => (e.clone(), theme::WARNING),
    }
}

impl Root {
    /// The fix notice: a small card in the window's corner, only while Recall has detected a
    /// fixable problem, an applied fix can still be undone (also after a restart, until its line
    /// is closed for this run), or a fix just gave a result.
    /// One short line per fix; the exact changes sit behind its (i). No list of past fixes and
    /// no instructions in the app (the user, 2026-11-12: "we either offer to fix and fix it if
    /// we detect the issue or we document on the project's github"; "we arent going to tell the
    /// user the instructions in such a text filled way").
    pub(crate) fn render_fix_notice(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (items, shown) = self.fix_notice_items(cx);
        if items.is_empty() {
            return None;
        }
        let show_close = (fixes::last().is_some() || !shown.is_empty()) && !fixes::running();
        let close = ui::button("fix-notice-close", Variant::Ghost).child(icon(Icon::X, 14., theme::TERTIARY)).on_click(cx.listener(move |_, _, _, cx| {
            // The result line goes; the "fixed" lines shown now stay closed until the next start.
            fixes::clear_last();
            close_receipts(shown.clone());
            cx.notify();
        }));
        Some(
            div()
                .absolute()
                .right(px(16.))
                .bottom(px(16.))
                .w(px(744.))
                .child(
                    card()
                        .gap(px(8.))
                        .border_color(c(theme::BORDER))
                        .bg(c(theme::RAISED))
                        .children(items)
                        .when(show_close, |d| d.child(div().flex().justify_end().child(close))),
                )
                .into_any_element(),
        )
    }

    /// The notice's lines, and the keys of the "fixed" lines among them (for close).
    fn fix_notice_items(&mut self, cx: &mut Context<Self>) -> (Vec<AnyElement>, Vec<String>) {
        let mut snap = fixes::snapshot();
        // FPS: stats on, and the recorder says FPS is refused for lack of the group.
        let fps_refused = self.stats_on() && self.snap.session.as_ref().is_some_and(|s| s.fps_source == "no_permission");
        snap.offers.extend(fixes::fps_offer(fps_refused));
        let last = fixes::last();
        snap.applied = shown_receipts(std::mem::take(&mut snap.applied), &closed_now());
        let shown: Vec<String> = snap.applied.iter().map(receipt_key).collect();
        if snap.offers.is_empty() && snap.applied.is_empty() && last.is_none() {
            return (Vec::new(), shown);
        }
        let busy = fixes::running();
        let mut items = Vec::new();
        for (i, offer) in snap.offers.iter().enumerate() {
            items.extend(self.offer_row(i, offer, busy, last.as_ref(), cx));
        }
        for (i, receipt) in snap.applied.iter().enumerate() {
            items.extend(self.applied_row(i, receipt, busy, last.as_ref(), cx));
        }
        // A failed apply leaves no offer or receipt behind once the offer is gone: keep its line.
        if let Some(l) = last.as_ref().filter(|l| !snap.offers.iter().any(|o| o.plan.fix == l.fix && o.target == l.target) && !snap.applied.iter().any(|r| r.fix == l.fix && r.target == l.target)) {
            match &l.result {
                RunResult::Failed(e) => items.push(self.failed_line(e, l, None, cx)),
                // An Undo just done: its row is gone, so say so here.
                RunResult::Done | RunResult::DoneWithNote(_) if l.action == Action::Undo => {
                    let text = match &l.result {
                        RunResult::DoneWithNote(n) => n.clone(),
                        _ => l.fix.after().map_or("Undone.".to_owned(), |a| format!("Undone. {a} for it to take effect.")),
                    };
                    items.push(div().text_size(px(12.)).text_color(c(theme::MUTED)).child(text).into_any_element());
                }
                _ => {}
            }
        }
        (items, shown)
    }

    fn info_button(&self, id: String, k: String, cx: &mut Context<Self>) -> AnyElement {
        let open = is_open(&k);
        div()
            .child(crate::trace::target(id.clone()))
            .id(SharedString::from(id))
            .size(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(10.))
            .cursor_pointer()
            .hover(|s| s.bg(c(theme::OVERLAY)))
            .when(open, |d| d.bg(c(theme::OVERLAY)))
            .child(icon(Icon::Info, 14., if open { theme::TEXT } else { theme::TERTIARY }))
            .on_click(cx.listener(move |_, _, _, cx| {
                toggle(&k);
                cx.notify();
            }))
            .into_any_element()
    }

    fn offer_row(&mut self, i: usize, offer: &Offer, busy: bool, last: Option<&Last>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let k = key(&offer.plan, &offer.target);
        let info = self.info_button(format!("fix-info-{i}"), k.clone(), cx);
        let (fix_offer, skip_offer) = (offer.clone(), offer.clone());
        let row = setting_row_with(offer.plan.fix.title(), Some(info), "")
            .child(ui::button(SharedString::from(format!("fix-not-now-{i}")), Variant::Ghost).child("Not now").on_click(cx.listener(move |_, _, _, cx| {
                fixes::dismiss(&skip_offer);
                cx.notify();
            })))
            .child(ui::button(SharedString::from(format!("fix-apply-{i}")), Variant::Primary).child(if busy { "Fixing…" } else { "Fix" }).on_click(cx.listener(move |_, _, window, cx| {
                // The app's window owns any administrator prompt, so it opens in front of Recall.
                fixes::apply(fix_offer.clone(), crate::root::parent_hwnd(window).unwrap_or(0));
                redraw_when_done(cx);
                cx.notify();
            })));
        let mut out = vec![row.into_any_element()];
        if is_open(&k) {
            out.push(self.plan_card(&offer.plan, &k, cx));
        }
        if let Some(l) = last.filter(|l| l.fix == offer.plan.fix && l.target == offer.target) {
            out.push(self.result_element(l, &offer.plan, cx));
        }
        out
    }

    fn applied_row(&mut self, i: usize, receipt: &Receipt, busy: bool, last: Option<&Last>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let k = key(&receipt.plan, &receipt.target);
        let info = self.info_button(format!("fix-applied-info-{i}"), k.clone(), cx);
        let undo = receipt.clone();
        let row = setting_row_with(&format!("{}: fixed", receipt.fix.title()), Some(info), "").child(
            ui::button(SharedString::from(format!("fix-undo-{i}")), Variant::Secondary).child(if busy { "Working…" } else { "Undo" }).on_click(cx.listener(move |_, _, window, cx| {
                fixes::undo(undo.clone(), crate::root::parent_hwnd(window).unwrap_or(0));
                redraw_when_done(cx);
                cx.notify();
            })),
        );
        let mut out = vec![row.into_any_element()];
        if is_open(&k) {
            out.push(self.plan_card(&receipt.plan, &k, cx));
        }
        if let Some(l) = last.filter(|l| l.fix == receipt.fix && l.target == receipt.target) {
            out.push(self.result_element(l, &receipt.plan, cx));
        }
        out
    }

    fn result_element(&mut self, last: &Last, plan: &Plan, cx: &mut Context<Self>) -> AnyElement {
        if let RunResult::Failed(e) = &last.result {
            return self.failed_line(e, last, Some(plan), cx);
        }
        let (text, color) = result_line(last, plan);
        div().text_size(px(12.)).text_color(c(color)).child(text).into_any_element()
    }

    /// The error in words, and "Report this" with the report page pre-filled.
    fn failed_line(&mut self, error: &str, last: &Last, plan: Option<&Plan>, cx: &mut Context<Self>) -> AnyElement {
        let report = fixes::report_text(last, plan);
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(div().flex_1().min_w_0().text_size(px(12.)).text_color(c(theme::WARNING)).child(error.to_owned()))
            .child(ui::button("fix-report", Variant::Secondary).child("Report this").on_click(cx.listener(move |this, _, _, cx| {
                let form = &mut this.settings_ui.report;
                let field = form.field.get_or_insert_with(|| crate::ui::TextField::new(cx, "What went wrong, and what were you doing?", 1500));
                field.set(report.clone());
                this.go(Route::Report);
                cx.notify();
            })))
            .into_any_element()
    }
}

impl Root {
    /// The (i) card: a few plain lines (what changes, that Undo puts it back, what to do
    /// after); the exact changes only behind "Details" (the user, 2026-11-12: "we arent going
    /// to tell the user the instructions in such a text filled way").
    fn plan_card(&self, plan: &Plan, k: &str, cx: &mut Context<Self>) -> AnyElement {
        let muted = |text: String| div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::MUTED)).child(text);
        let mut lines = div().flex_1().min_w_0().flex().flex_col().gap(px(6.));
        for step in &plan.steps {
            lines = lines.child(div().font_weight(gpui::FontWeight::MEDIUM).child(step.change.summary()));
        }
        lines = lines.child(muted("Undo puts it back.".to_owned()));
        if let Some(after) = plan.fix.after() {
            lines = lines.child(muted(format!("Afterwards: {after}.")));
        }
        let details_key = format!("{k}|details");
        let details = details_open(&details_key);
        let toggle_key = details_key.clone();
        lines = lines.child(
            ui::button(SharedString::from(format!("fix-details-{k}")), Variant::Ghost)
                .child(if details { "Hide details" } else { "Details" })
                .on_click(cx.listener(move |_, _, _, cx| {
                    toggle(&toggle_key);
                    cx.notify();
                })),
        );
        if details {
            for step in &plan.steps {
                for line in step.change.exact() {
                    lines = lines.child(muted(line));
                }
            }
        }
        // The setting rows' frame (a flex row holding a flex_1 + min_w_0 column) is what lets text
        // wrap and take its wrapped height; a plain column measured the lines unwrapped and they
        // spilled out of the card. 712 = the settings column (760) less its side padding.
        card().w(px(712.)).max_w_full().flex_none().child(div().flex().child(lines)).into_any_element()
    }
}

/// Whether the exact-changes list under an (i) card is open: only by its own toggle (layout
/// pictures that open every (i) still show the short card).
fn details_open(k: &str) -> bool {
    OPEN.lock().unwrap_or_else(|p| p.into_inner()).as_ref().is_some_and(|s| s.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use recall_fixes::FixId;

    fn plan() -> Plan {
        Plan { fix: FixId::MinecraftOverlayNvidia, steps: Vec::new() }
    }

    fn last(action: Action, result: RunResult) -> Last {
        Last { fix: FixId::MinecraftOverlayNvidia, target: r"C:\j\javaw.exe".into(), action, result }
    }

    /// Rohan 2026-11-12 (b): an applied fix keeps its Undo after a restart, also when the fix is
    /// no longer offered; close hides it for this run only.
    #[test]
    fn an_applied_fix_shows_at_start_until_closed_this_run() {
        let receipt = Receipt { fix: FixId::MinecraftOverlayNvidia, target: r"C:\g\jre25\bin\javaw.exe".into(), applied_unix: 1, plan: plan(), undo: Vec::new() };
        assert!(!receipt.fix.offered(), "a fix that is off");
        let none = HashSet::new();
        assert_eq!(shown_receipts(vec![receipt.clone()], &none).len(), 1, "shown at start");
        let closed: HashSet<String> = [receipt_key(&receipt)].into();
        assert!(shown_receipts(vec![receipt.clone()], &closed).is_empty(), "not again this run after close");
    }

    #[test]
    fn results_read_plainly() {
        assert_eq!(result_line(&last(Action::Apply, RunResult::Done), &plan()).0, "Fixed. Restart Minecraft for it to take effect.");
        assert_eq!(result_line(&last(Action::Undo, RunResult::Done), &plan()).0, "Undone. Restart Minecraft for it to take effect.");
        assert!(result_line(&last(Action::Apply, RunResult::Declined), &plan()).0.starts_with("Nothing changed"));
        let (text, color) = result_line(&last(Action::Apply, RunResult::Failed("x failed".into())), &plan());
        assert_eq!((text.as_str(), color), ("x failed", theme::WARNING));
    }

    #[test]
    fn the_info_list_opens_and_closes_per_fix_and_program() {
        let k = key(&plan(), r"C:\J\JAVAW.EXE");
        assert_eq!(k, key(&plan(), r"c:\j\javaw.exe"));
        assert!(!is_open(&k));
        toggle(&k);
        assert!(is_open(&k));
        toggle(&k);
        assert!(!is_open(&k));
        assert_eq!(short(r"C:\j\bin\javaw.exe"), "javaw.exe");
    }
}

/// Redraw once a Fix or Undo started from the notice ends (it may wait on Windows' prompt).
fn redraw_when_done(cx: &mut Context<Root>) {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
            if !fixes::running() {
                break;
            }
        }
        let _ = this.update(cx, |_, cx| cx.notify());
    })
    .detach();
}
