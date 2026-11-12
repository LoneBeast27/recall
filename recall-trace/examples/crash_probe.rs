//! Crash-record probe: `crash_probe panic|native` writes a trace and then crashes the chosen
//! way, so a check can read the record it leaves in `RECALL_TRACE_DIR` (default: a
//! `recall-trace-probe` folder under the temp folder). A native crash leaves
//! `crash-probe-native.pending`; the next probe start files it as a numbered record.
//! No error box: the probe turns off Windows' crash dialog for itself first.

fn main() {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn SetErrorMode(mode: u32) -> u32;
        }
        // SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX
        unsafe { SetErrorMode(0x0001 | 0x0002) };
    }
    // Never the user's trace folder: a temp folder unless the caller names one.
    if std::env::var_os("RECALL_TRACE_DIR").is_none() {
        std::env::set_var("RECALL_TRACE_DIR", std::env::temp_dir().join("recall-trace-probe"));
    }
    eprintln!("crash_probe: records in {}", recall_trace::run_dir().map(|d| d.display().to_string()).unwrap_or_default());
    recall_trace::start("probe");
    recall_trace::act("probe.armed", &[("step", 1u32.into())]);
    match std::env::args().nth(1).as_deref() {
        Some("panic") => panic!("probe panic with a path C:\\Users\\someone\\clip.mp4"),
        Some("native") => unsafe {
            let p: *mut u32 = std::ptr::null_mut();
            std::ptr::write_volatile(p, 1);
        },
        _ => eprintln!("usage: crash_probe panic|native"),
    }
}
