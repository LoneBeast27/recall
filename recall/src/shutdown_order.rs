//! The engine's place in the Windows sign-out / shutdown order. At a session end Windows
//! asks processes in order of their shutdown level, highest first. The controller holds
//! the session while the engine still writes a save (`recall-hdr` `record_session_end`)
//! and then quits the engine itself; the engine is a console process with its own
//! notices, so without this it could be ended beside the controller, mid-save. A level
//! below the default (0x280) puts it after the controller and every ordinary app.

/// The engine's shutdown level: after the default 0x280 (the controller), well above
/// the system's own range (below 0x100 is reserved).
pub const ENGINE_SHUTDOWN_LEVEL: u32 = 0x100;

#[link(name = "kernel32")]
extern "system" {
    fn SetProcessShutdownParameters(level: u32, flags: u32) -> i32;
    #[cfg_attr(not(test), allow(dead_code))]
    fn GetProcessShutdownParameters(level: *mut u32, flags: *mut u32) -> i32;
}

/// Put this process after the controller in the shutdown order (logged on failure).
pub fn apply() {
    // SAFETY: plain call, no pointers.
    if unsafe { SetProcessShutdownParameters(ENGINE_SHUTDOWN_LEVEL, 0) } == 0 {
        eprintln!("[recall] SetProcessShutdownParameters failed: {}", std::io::Error::last_os_error());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_is_asked_to_end_after_the_controller() {
        // R6-overlay-6. Changes the test process's own shutdown order only.
        apply();
        let (mut level, mut flags) = (0u32, 0u32);
        // SAFETY: two valid out pointers.
        assert_ne!(unsafe { GetProcessShutdownParameters(&mut level, &mut flags) }, 0);
        assert_eq!(level, ENGINE_SHUTDOWN_LEVEL);
        assert!(level < 0x280, "after the default level the controller runs at");
    }
}
