mod app;
mod audio;
mod backend;
#[path = "../../recall/src/edit_model.rs"]
mod edit_model;
mod icons;
mod media;
mod services;
mod theme;
fn main() {
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
        std::process::exit(1);
    }
}
