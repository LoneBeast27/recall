// Release builds are GUI programs: started by hand or by a script, they open no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
const TESTING: bool = true;
include!("entry.rs");
