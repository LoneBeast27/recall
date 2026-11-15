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
    rl_offered: Arc<std::sync::atomic::AtomicBool>,
}

impl FixHints {
    /// The game hides the island: check once per game folder per run, off the pump thread.
    pub fn island_hidden(&self, target: &Target) {
        if crate::fix_rl::is_game(&target.exe) {
            return self.rocket_league();
        }
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

    /// Rocket League in its Fullscreen mode: offer its Borderless mode (read-only check, once
    /// per run, off the pump thread; only while Recall offers the fix at all).
    fn rocket_league(&self) {
        let fix = FixId::RocketLeagueBorderless;
        if !fix.offered() {
            return;
        }
        let Some(config) = crate::fix_rl::config_path().map(|p| p.to_string_lossy().into_owned()) else { return };
        if !self.checked.lock().unwrap_or_else(|p| p.into_inner()).insert(config.to_lowercase()) {
            return;
        }
        let rl_offered = self.rl_offered.clone();
        std::thread::spawn(move || {
            let id = target_hash(&config);
            let Some(dir) = fixes_dir() else { return };
            if Dismissed::new(&dir).contains(fix, &config) {
                log!("fixes: {} for settings file {id}: the user said Not now; no offer", fix.as_str());
                return;
            }
            match crate::fix_rl::plan(&config) {
                Ok(crate::fix_rl::Check::Offer(plan)) => match Offer::new(plan, &config).save(&dir.join("offers")) {
                    Ok(_) => {
                        log!("fixes: offered {} for settings file {id}", fix.as_str());
                        rl_offered.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(e) => log!("fixes: offer not saved: {e}"),
                },
                Ok(crate::fix_rl::Check::AlreadyFixed) => log!("fixes: settings file {id} already asks for Borderless; no offer"),
                Ok(crate::fix_rl::Check::NotApplicable(why)) => log!("fixes: no {} offer: {why}", fix.as_str()),
                Err(e) => log!("fixes: no {} offer: {e}", fix.as_str()),
            }
        });
    }

    /// The Rocket League fix was offered this run: the island's notice points at it.
    pub fn rocket_league_offered(&self) -> bool {
        self.rl_offered.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// A Minecraft fix was offered this run: the island's notice names it
    /// instead of "set it to Borderless" (Minecraft hides the island there too).
    pub fn minecraft_offered(&self) -> bool {
        !self.offered.lock().unwrap_or_else(|p| p.into_inner()).is_empty()
    }
}
