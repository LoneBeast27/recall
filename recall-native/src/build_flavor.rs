//! The build flavour of every crate root except the RecallTesting binary (`testing_main.rs`,
//! which keeps its own `TESTING = true`). Recall (`main.rs`) and every example re-export this
//! as `crate::TESTING`: the shared modules they `#[path]`-include read `crate::TESTING`, so an
//! example root without it does not compile (11-09: three examples broke that way, and a
//! `--bin`-only check hid it).
pub(crate) const TESTING: bool = false;
