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
    views::pages::{card, section_title, setting_row_with},
};
use recall_fixes::offer::Offer;
use recall_fixes::run::Result as RunResult;
use recall_fixes::{Plan, Receipt};

/// Which (i) lists are open, by fix and program (a view detail, not a setting).
static OPEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn key(plan: &Plan, target: &str) -> String {
    format!("{}|{}", plan.fix.as_str(), target.to_lowercase())
}

fn is_open(k: &str) -> bool {
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
    /// The Fixes section; nothing when there is nothing to offer or undo.
    pub(crate) fn settings_fixes(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let snap = fixes::snapshot();
        let last = fixes::last();
        if snap.offers.is_empty() && snap.applied.is_empty() && last.is_none() {
            return Vec::new();
        }
        let busy = fixes::running();
        let mut items = vec![section_title("Fixes")];
        for (i, offer) in snap.offers.iter().enumerate() {
            items.extend(self.offer_row(i, offer, busy, last.as_ref(), cx));
        }
        for (i, receipt) in snap.applied.iter().enumerate() {
            items.extend(self.applied_row(i, receipt, busy, last.as_ref(), cx));
        }
        // A failed apply leaves no offer or receipt behind once the offer is gone: keep its line.
        if let Some(l) = last.as_ref().filter(|l| !snap.offers.iter().any(|o| o.plan.fix == l.fix && o.target == l.target) && !snap.applied.iter().any(|r| r.fix == l.fix && r.target == l.target)) {
            if let RunResult::Failed(e) = &l.result {
                items.push(self.failed_line(e, l, None, cx));
            }
        }
        items
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
        let row = setting_row_with(offer.plan.fix.title(), Some(info), offer.plan.fix.problem())
            .child(ui::button(SharedString::from(format!("fix-not-now-{i}")), Variant::Ghost).child("Not now").on_click(cx.listener(move |_, _, _, cx| {
                fixes::dismiss(&skip_offer);
                cx.notify();
            })))
            .child(ui::button(SharedString::from(format!("fix-apply-{i}")), Variant::Primary).child(if busy { "Fixing…" } else { "Fix" }).on_click(cx.listener(move |_, _, _, cx| {
                fixes::apply(fix_offer.clone());
                cx.notify();
            })));
        let mut out = vec![row.into_any_element()];
        if is_open(&k) {
            out.push(plan_card(&offer.plan));
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
        let detail = format!("Applied to {}. Undo puts the old values back.", short(&receipt.target));
        let row = setting_row_with(&format!("{} (applied)", receipt.fix.title()), Some(info), &detail).child(
            ui::button(SharedString::from(format!("fix-undo-{i}")), Variant::Secondary).child(if busy { "Working…" } else { "Undo" }).on_click(cx.listener(move |_, _, _, cx| {
                fixes::undo(undo.clone());
                cx.notify();
            })),
        );
        let mut out = vec![row.into_any_element()];
        if is_open(&k) {
            out.push(plan_card(&receipt.plan));
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

/// The (i) list: each change in one plain line, then its exact lines.
fn plan_card(plan: &Plan) -> AnyElement {
    let mut body = card().gap(px(6.));
    for step in &plan.steps {
        body = body.child(div().font_weight(gpui::FontWeight::MEDIUM).child(step.change.summary()));
        for line in step.change.exact() {
            body = body.child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::MUTED)).child(line));
        }
    }
    if let Some(after) = plan.fix.after() {
        body = body.child(div().text_size(px(12.)).text_color(c(theme::MUTED)).child(format!("Afterwards: {after}.")));
    }
    body.into_any_element()
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
