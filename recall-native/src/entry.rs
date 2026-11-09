mod app;
#[cfg(test)]
mod test_temp;
mod assets;
mod audio;
mod backend;
mod cache;
mod clip_open;
#[path = "../../recall/src/edit_model.rs"]
mod edit_model;
// Can a clip really go to the Recycle Bin (shared with the recorder's storage limit).
#[allow(dead_code)]
#[path = "../../recall/src/win_recycle.rs"]
mod win_recycle;
#[allow(dead_code)]
#[path = "../../recall/src/win_fileop.rs"]
mod win_fileop;
#[allow(dead_code)]
#[path = "../../recall/src/win_volume.rs"]
mod win_volume;
// Which file a path names, by volume serial + file index (shared with the engine's mover).
#[allow(dead_code)]
#[path = "../../recall/src/file_id.rs"]
mod file_id;
mod editor_state;
mod install_wait;
mod instance;
mod library_probe;
mod display_tiles;
mod library_state;
mod media;
mod offscreen;
mod playback_watch;
mod root;
mod services;
#[path = "services/update.rs"]
mod update;
mod shell;
mod ssd_meter;
mod startup;
mod theme;
mod trace;
mod transport_rules;
mod ui;
mod views;
mod worker;
mod worker_lanes;

const APP_NAME: &str = if TESTING { "RecallTesting" } else { "Recall" };

fn main() {
    if TESTING {
        if let Err(error) = app::run(None) {
            eprintln!("{APP_NAME}: {error}");
            std::process::exit(1);
        }
        return;
    }
    startup::mark("main");
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
    // An install replacing files (`.installing` beside the program): wait for it first.
    install_wait::wait_for_install();
    // One library per user: a second start shows the running window and exits.
    let guard = match instance::start() {
        instance::Start::Run(guard) => guard,
        instance::Start::Exit => std::process::exit(0),
    };
    startup::mark("instance guard");
    if let Err(error) = app::run(guard) {
        eprintln!("Recall native: {error}");
        trace::player_log("start failed", serde_json::json!({ "error": error }));
        std::process::exit(1);
    }
}
