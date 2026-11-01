// Release builds are GUI programs: started by hand or by a script, they open no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app;
mod assets;
mod audio;
mod backend;
mod cache;
#[path = "../../recall/src/edit_model.rs"]
mod edit_model;
mod editor_state;
mod library_state;
mod media;
mod offscreen;
mod playback_watch;
mod root;
mod services;
mod shell;
mod theme;
mod trace;
mod transport_rules;
mod ui;
mod views;
mod worker;
fn main() {
    // A GUI program has no stderr of its own: print to the terminal it was started from, if any,
    // and keep panics in the player log for a start from the Start menu.
    let _ = unsafe { windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS) };
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        trace::player_log("panic", serde_json::json!({ "info": info.to_string() }));
        default_panic(info);
    }));
    if let Ok(mask) = std::env::var("RECALL_CPU_MASK") {
        use windows::Win32::System::Threading::{
            BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass,
            SetProcessAffinityMask,
        };
        let mask = usize::from_str_radix(mask.trim_start_matches("0x"), 16).unwrap_or(0);
        if mask == 0 || unsafe { SetProcessAffinityMask(GetCurrentProcess(), mask) }.is_err() {
            eprintln!("Recall native CPU affinity failed");
            std::process::exit(2);
        }
        let _ = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
    }
    if let Err(error) = app::run() {
        eprintln!("Recall native: {error}");
        trace::player_log("start failed", serde_json::json!({ "error": error }));
        std::process::exit(1);
    }
}
