//! The controller-owned `recall-overlay.exe` (Recall Island) process: spawned
//! with `--corner <c> --parent <controller pid>` while notices
//! or the stats island are visible, restarted when the corner changes,
//! stopped when hidden, and killed on quit. The overlay
//! also exits by itself when its `--parent` dies, so a hard-killed controller
//! never leaves it behind. A crash-looping overlay is respawned at most every
//! [`RESPAWN_DELAY`] and abandoned after [`MAX_QUICK_EXITS`] quick exits.
//! An overlay that is killed or dies cannot stop its frame-timing ETW
//! session, so the controller stops it by name then (`frame_timing`).

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::log;
use crate::record_config::OverlayCfg;

const RESPAWN_DELAY: Duration = Duration::from_secs(5);
/// An overlay that dies within this long of starting counts as a quick exit.
const QUICK_EXIT: Duration = Duration::from_secs(10);
const MAX_QUICK_EXITS: u32 = 5;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The overlay's command-line arguments.
pub fn overlay_args(corner: &str, parent_pid: u32) -> Vec<String> {
    vec!["--corner".into(), corner.into(), "--parent".into(), parent_pid.to_string()]
}

pub struct OverlayProc {
    exe: Option<PathBuf>,
    child: Option<(Child, String, Instant)>,
    quick_exits: u32,
    not_before: Option<Instant>,
}

impl OverlayProc {
    /// Locate `recall-overlay.exe` next to the controller (or under the dev
    /// `target/{debug,release}` dirs, like the engine).
    pub fn new() -> Self {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|me| crate::supervisor::locate_sibling_from(&me, "recall-overlay.exe"));
        if exe.is_none() {
            log!("overlay: recall-overlay.exe not found; overlay toasts unavailable");
        }
        OverlayProc {
            exe,
            child: None,
            quick_exits: 0,
            not_before: None,
        }
    }

    /// Whether the overlay process is alive right now.
    pub fn running(&mut self) -> bool {
        self.reap();
        self.child.is_some()
    }

    /// Start, restart, or stop the process to match the current overlay settings.
    pub fn sync(&mut self, cfg: &OverlayCfg) {
        self.reap();
        if !cfg.wants_process() {
            self.stop();
            self.quick_exits = 0;
            return;
        }
        if let Some((_, corner, _)) = &self.child {
            if *corner == cfg.corner {
                return;
            }
            log!("overlay: corner {corner} -> {}; restarting", cfg.corner);
            self.stop();
            self.quick_exits = 0;
            self.not_before = None;
        }
        if self.quick_exits >= MAX_QUICK_EXITS
            || self.not_before.is_some_and(|t| Instant::now() < t)
        {
            return;
        }
        self.spawn(&cfg.corner);
    }

    fn spawn(&mut self, corner: &str) {
        use std::os::windows::process::CommandExt;
        let Some(exe) = &self.exe else { return };
        match Command::new(exe)
            .args(overlay_args(corner, std::process::id()))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(child) => {
                log!("overlay: started (pid {}, corner {corner})", child.id());
                self.child = Some((child, corner.to_string(), Instant::now()));
            }
            Err(e) => {
                log!("overlay: spawn failed: {e}");
                self.quick_exits += 1;
                self.not_before = Some(Instant::now() + RESPAWN_DELAY);
            }
        }
    }

    /// Notice an overlay that exited on its own and schedule the respawn.
    fn reap(&mut self) {
        let exited = match self.child.as_mut() {
            Some((child, _, _)) => !matches!(child.try_wait(), Ok(None)),
            None => false,
        };
        if exited {
            if let Some((_, _, started)) = self.child.take() {
                if started.elapsed() < QUICK_EXIT {
                    self.quick_exits += 1;
                } else {
                    self.quick_exits = 0;
                }
                log!("overlay: exited by itself ({} quick exit(s))", self.quick_exits);
                crate::frame_timing::stop_session();
                if self.quick_exits >= MAX_QUICK_EXITS {
                    log!("overlay: giving up after {MAX_QUICK_EXITS} quick exits");
                }
                self.not_before = Some(Instant::now() + RESPAWN_DELAY);
            }
        }
    }

    /// Stop the overlay we spawned (our own child only).
    pub fn stop(&mut self) {
        if let Some((mut child, _, _)) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            log!("overlay: stopped");
            crate::frame_timing::stop_session();
        }
    }
}

impl Drop for OverlayProc {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_args_name_corner_and_parent() {
        assert_eq!(overlay_args("bottom-left", 4242), ["--corner", "bottom-left", "--parent", "4242"]);
    }

    #[test]
    fn disabled_or_missing_exe_never_spawns() {
        let mut o = OverlayProc {
            exe: None,
            child: None,
            quick_exits: 0,
            not_before: None,
        };
        o.sync(&OverlayCfg { enabled: true, ..OverlayCfg::default() });
        assert!(!o.running(), "no exe -> nothing spawned");
        o.sync(&OverlayCfg::default());
        assert!(!o.running());
    }

    #[test]
    fn notices_or_stats_want_the_process() {
        let off = OverlayCfg::default();
        assert!(!off.wants_process());
        assert!(OverlayCfg { enabled: true, ..off.clone() }.wants_process());
        assert!(OverlayCfg { stats: true, ..off.clone() }.wants_process(), "stats alone start the overlay");
        assert!(OverlayCfg { enabled: true, stats: true, ..off.clone() }.wants_process());
        assert!(!OverlayCfg { hidden: true, enabled: true, stats: true, ..off }.wants_process());
    }
}
