//! Tests: what this thread emitted. `emit`'s one observer (`set_observer`, first call wins) is
//! installed by [`test_emits::install`] and keeps each thread's lines apart, so tests that
//! emit in parallel do not see each other's.
use std::cell::RefCell;

thread_local! {
    static LINES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Start collecting (idempotent) and clear this thread's lines.
pub(crate) fn install() {
    super::set_observer(|msg| {
        if let Ok(line) = serde_json::to_string(msg) {
            LINES.with(|l| l.borrow_mut().push(line));
        }
    });
    LINES.with(|l| l.borrow_mut().clear());
}

/// This thread's lines since [`install`] whose JSON `type` is `kind`.
pub(crate) fn of(kind: &str) -> Vec<String> {
    let tag = format!(r#""type":"{kind}""#);
    LINES.with(|l| l.borrow().iter().filter(|line| line.contains(&tag)).cloned().collect())
}
