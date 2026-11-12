//! Fix offers the recorder finds while a game runs. When Minecraft Java hides the island
//! (Windows reports it as owning the display), a background thread looks at that game's own
//! folder (read-only): if it uses the Cubes Without Borders mod, whose default borderless
//! window is exactly the screen size, and the user has not said Not now, it writes an offer
//! file the Recall app shows: `%LOCALAPPDATA%\recall-hdr\fixes\offers\<fix>-<hash>.json`.
//! Nothing on the PC changes here; the app applies a fix only when the user clicks it.
//! (The NVIDIA driver-profile fix is kept for receipts and Undo but no longer offered: the
//! driver applied it in only some launches.) Log lines name programs by hash only.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use recall_fixes::offer::{fixes_dir, Dismissed, Offer};
use recall_fixes::{target_hash, FixId};

use crate::fix_cwb::Check;
use crate::log;
use crate::record_target::{Target, MINECRAFT};

const FIX: FixId = FixId::MinecraftBorderlessCwb;

/// Off for now: the Minecraft fix is documented on the project's known-issues page until a
/// fix proves reliable in testing (the user, 2026-11-12). Detection stays ready behind this.
const OFFER_MINECRAFT: bool = false;

/// Game folders checked this run (lower-case) and whether one got an offer.
#[derive(Default, Clone)]
pub struct FixHints {
    checked: Arc<Mutex<HashSet<String>>>,
    offered: Arc<Mutex<HashSet<String>>>,
}

impl FixHints {
    /// The game hides the island: check once per game folder per run, off the pump thread.
    pub fn island_hidden(&self, target: &Target) {
        if !OFFER_MINECRAFT || target.name != MINECRAFT || !crate::fix_nvidia::is_java(&target.exe) {
            return;
        }
        let pid = target.pid;
        let checked = self.checked.clone();
        let offered = self.offered.clone();
        std::thread::spawn(move || {
            let Some(instance) = crate::watcher::command_line_of(pid).and_then(|c| crate::fix_cwb::instance_of(&c)) else {
                log!("fixes: Minecraft's game folder is not on its command line; no offer");
                return;
            };
            let config = crate::fix_cwb::config_path(&instance).to_string_lossy().into_owned();
            if !checked.lock().unwrap_or_else(|p| p.into_inner()).insert(config.to_lowercase()) {
                return;
            }
            let id = target_hash(&config);
            let Some(dir) = fixes_dir() else { return };
            if Dismissed::new(&dir).contains(FIX, &config) {
                log!("fixes: {} for game folder {id}: the user said Not now; no offer", FIX.as_str());
                return;
            }
            match crate::fix_cwb::plan(&config) {
                Ok(Check::Offer(plan)) => match Offer::new(plan, &config).save(&dir.join("offers")) {
                    Ok(_) => {
                        log!("fixes: offered {} for game folder {id}", FIX.as_str());
                        offered.lock().unwrap_or_else(|p| p.into_inner()).insert(config);
                    }
                    Err(e) => log!("fixes: offer not saved: {e}"),
                },
                Ok(Check::AlreadyFixed) => log!("fixes: game folder {id} already uses Windows' borderless; no offer"),
                Ok(Check::NotApplicable(_)) => log!("fixes: game folder {id} does not use Cubes Without Borders (or its config is unreadable); no offer"),
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
