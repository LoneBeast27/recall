//! Shortcuts the OS refused at bind time (another app held the combo) are tried again every
//! [`RETRY_EVERY`] until they register, so a shortcut freed by an app that quit starts working
//! and Settings stops saying "Not working". Parse errors and duplicates are not retried: only
//! a config change can fix those.
use std::time::{Duration, Instant};

use global_hotkey::hotkey::HotKey;

use crate::record_hotkeys::Binding;

pub const RETRY_EVERY: Duration = Duration::from_secs(10);

/// The refused bindings with the OS error, and when to try them next.
#[derive(Default)]
pub struct Refused {
    list: Vec<(Binding, HotKey, String)>,
    next: Option<Instant>,
}

impl Refused {
    /// Replace the refused list (a fresh bind); the first retry is one interval away.
    pub fn set(&mut self, list: Vec<(Binding, HotKey, String)>, now: Instant) {
        self.next = (!list.is_empty()).then(|| now + RETRY_EVERY);
        self.list = list;
    }

    /// A retry is due (something is refused and the interval has passed).
    pub fn due(&self, now: Instant) -> bool {
        self.next.is_some_and(|at| now >= at)
    }

    /// Try each refused accelerator again; returns the ones that registered. The rest wait
    /// another interval (their error text is refreshed).
    pub fn retry(&mut self, now: Instant, mut register: impl FnMut(HotKey) -> Result<(), String>) -> Vec<(Binding, HotKey)> {
        let mut bound = Vec::new();
        let mut still = Vec::new();
        for (b, hk, _) in self.list.drain(..) {
            match register(hk) {
                Ok(()) => bound.push((b, hk)),
                Err(e) => still.push((b, hk, e)),
            }
        }
        self.set(still, now);
        bound
    }

    /// One "could not be registered" line per refused binding (status `hotkey_problems`).
    pub fn warnings(&self) -> Vec<String> {
        self.list.iter().map(|(b, _, e)| refusal_text(b, e)).collect()
    }
}

/// The warning for an accelerator the OS refused.
pub fn refusal_text(b: &Binding, error: &str) -> String {
    format!("{} hotkey '{}' could not be registered (another app may own it): {error}", b.label, b.accel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::parse_accel;
    use crate::record_hotkeys::HotkeyAction;

    fn save() -> (Binding, HotKey, String) {
        let b = Binding { action: HotkeyAction::Save { seconds: 0 }, key: "recording.save_hotkeys[0]".into(), label: "save (full buffer)".into(), accel: "Alt+N".into() };
        let hk = parse_accel("Alt+N").unwrap();
        (b, hk, "already registered".into())
    }

    #[test]
    fn a_refused_shortcut_registers_once_the_other_app_lets_go() {
        let t0 = Instant::now();
        let mut refused = Refused::default();
        refused.set(vec![save()], t0);
        assert!(!refused.due(t0 + Duration::from_secs(5)), "not before the retry interval");
        assert!(refused.due(t0 + RETRY_EVERY), "due after it");
        let mut calls = 0;
        // The other app still holds it on the first retry, then quits.
        let mut register = |_hk: HotKey| {
            calls += 1;
            if calls == 1 { Err("held".to_string()) } else { Ok(()) }
        };
        assert!(refused.retry(t0 + RETRY_EVERY, &mut register).is_empty());
        assert_eq!(refused.warnings().len(), 1, "still reported while refused");
        assert!(!refused.due(t0 + RETRY_EVERY + Duration::from_secs(1)), "the next try waits again");
        let now = t0 + RETRY_EVERY * 2;
        assert!(refused.due(now));
        let bound: Vec<HotkeyAction> = refused.retry(now, &mut register).into_iter().map(|(b, _)| b.action).collect();
        assert_eq!(bound, vec![HotkeyAction::Save { seconds: 0 }]);
        assert!(refused.warnings().is_empty(), "Settings stops saying Not working");
        assert!(!refused.due(now + RETRY_EVERY * 5), "nothing left to retry");
    }
}
