//! Crash-record probe: `crash_probe panic|native` with `RECALL_TRACE_DIR=<temp folder>` writes a
//! trace and then crashes the chosen way, so a check can read the crash record it leaves.
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
