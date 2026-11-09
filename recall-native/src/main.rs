// Release builds are GUI programs: started by hand or by a script, they open no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod build_flavor;
#[cfg(test)]
mod hw_gate;
pub(crate) use build_flavor::TESTING;
include!("entry.rs");
