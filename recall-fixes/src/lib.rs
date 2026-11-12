//! The in-app fixer: Recall spots a known problem that has a known fix and
//! offers the fix. Nothing changes until the user clicks Fix for that one fix.
//!
//! - [`plan`]: what a fix changes, as data. The "i" view is rendered from the
//!   same [`plan::Change`] values the helper executes, so the list the user
//!   reads cannot drift from what the code does.
//! - [`helper`]: the command line of the helper that applies one change.
//!   Every change runs in that helper; one that needs an administrator starts
//!   it with the shell's `runas` verb so Windows shows its own prompt
//!   ([`elevate`]). The helper takes only known fix IDs and checks every
//!   argument again.
//! - [`receipt`]: every applied fix leaves a receipt holding the old values;
//!   Undo replays it in reverse.
//!
//! Vendor-neutral: vendor code (the NVIDIA driver database today) lives in its
//! own crate and is wired in by the program that runs the helper.

pub mod elevate;
pub mod helper;
pub mod offer;
pub mod plan;
pub mod receipt;
pub mod run;

pub use plan::{Change, DriverAppSetting, FixId, Plan, Profile, Step, Vendor};
pub use receipt::{DriverPrior, Receipt, Store, Undo};

#[cfg(test)]
mod tests;
