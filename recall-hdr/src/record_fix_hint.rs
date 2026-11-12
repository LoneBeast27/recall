//! Fix offers the recorder finds while a game runs. When Minecraft Java hides
//! the island (Windows reports it as owning the display, in borderless and in
//! fullscreen), a background thread reads the NVIDIA driver profile
//! (read-only) and, if the fix applies and the user has not said Not now to
//! it, writes an offer file the Recall app shows:
//! `%LOCALAPPDATA%\recall-hdr\fixes\offers\<fix>-<hash>.json`. Nothing on the
//! PC changes here; the app applies a fix only when the user clicks it.
//! Log lines name the program by hash only (its path holds the user name).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use recall_fixes::offer::{fixes_dir, Dismissed, Offer};
use recall_fixes::{target_hash, FixId};

use crate::fix_nvidia::Check;
use crate::log;
use crate::record_target::{Target, MINECRAFT};

const FIX: FixId = FixId::MinecraftOverlayNvidia;

/// Programs checked this run (lower-case path) and whether one got an offer.
#[derive(Default, Clone)]
pub struct FixHints {
    checked: Arc<Mutex<HashSet<String>>>,
    offered: Arc<Mutex<HashSet<String>>>,
}

impl FixHints {
    /// The game hides the island: check once per program per run, off the pump thread.
    pub fn island_hidden(&self, target: &Target) {
        if target.name != MINECRAFT || !crate::fix_nvidia::is_java(&target.exe) {
            return;
        }
        let Some(path) = crate::record_tray::process_path(target.pid) else { return };
        let exe = path.to_string_lossy().into_owned();
        if !self.checked.lock().unwrap_or_else(|p| p.into_inner()).insert(exe.clone()) {
            return;
        }
        let offered = self.offered.clone();
        std::thread::spawn(move || {
            let id = target_hash(&exe);
            let Some(dir) = fixes_dir() else { return };
            if Dismissed::new(&dir).contains(FIX, &exe) {
                log!("fixes: {} for program {id}: the user said Not now; no offer", FIX.as_str());
                return;
            }
            match crate::fix_nvidia::minecraft_plan(&exe) {
                Ok(Check::Offer(plan)) => match Offer::new(plan, &exe).save(&dir.join("offers")) {
                    Ok(_) => {
                        log!("fixes: offered {} for program {id}", FIX.as_str());
                        offered.lock().unwrap_or_else(|p| p.into_inner()).insert(exe);
                    }
                    Err(e) => log!("fixes: offer not saved: {e}"),
                },
                Ok(Check::AlreadyFixed) => log!("fixes: program {id} already has Recall's NVIDIA profile; no offer"),
                Ok(Check::OtherProfile(_)) => log!("fixes: program {id} belongs to an existing NVIDIA profile, which Recall never edits; no offer"),
                // No NVIDIA driver, or the database could not be read: no offer.
                Err(e) => log!("fixes: no {} offer: {e}", FIX.as_str()),
            }
        });
    }

    /// A Minecraft fix was offered this run: the island's notice names it
    /// instead of "set it to Borderless" (Minecraft hides the island there too).
    pub fn minecraft_offered(&self) -> bool {
        !self.offered.lock().unwrap_or_else(|p| p.into_inner()).is_empty()
    }
}
