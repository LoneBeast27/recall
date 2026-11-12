//! Fix offers the recorder finds while a game runs. When Minecraft Java hides
//! the island (Windows reports it as owning the display, in borderless and in
//! fullscreen), a background thread reads the NVIDIA driver profile
//! (read-only) and, if the fix applies, writes an offer file the Recall app
//! shows: `%LOCALAPPDATA%\recall-hdr\fixes\offers\<fix>-<hash>.json`. Nothing
//! on the PC changes here; the app applies a fix only when the user clicks it.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use recall_fixes::offer::Offer;

use crate::log;
use crate::record_target::{Target, MINECRAFT};

/// Programs checked this run (lower-case path) and those that got an offer.
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
        std::thread::spawn(move || match crate::fix_nvidia::minecraft_plan(&exe) {
            Ok(Some(plan)) => match Offer::default_dir().map(|d| Offer::new(plan, &exe).save(&d)) {
                Some(Ok(_)) => {
                    log!("fixes: offered \"{}\" for {exe}", recall_fixes::FixId::MinecraftOverlayNvidia.as_str());
                    offered.lock().unwrap_or_else(|p| p.into_inner()).insert(exe);
                }
                Some(Err(e)) => log!("fixes: offer not saved: {e}"),
                None => log!("fixes: offer not saved: LOCALAPPDATA is not set"),
            },
            Ok(None) => log!("fixes: {exe} already has the NVIDIA present method set; no offer"),
            // No NVIDIA driver, or the database could not be read: no offer.
            Err(e) => log!("fixes: no Minecraft offer: {e}"),
        });
    }

    /// A Minecraft fix was offered this run: the island's notice names it
    /// instead of "set it to Borderless" (Minecraft hides the island there too).
    pub fn minecraft_offered(&self) -> bool {
        !self.offered.lock().unwrap_or_else(|p| p.into_inner()).is_empty()
    }
}
