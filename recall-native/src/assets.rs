//! Embedded SVG icons served to GPUI's `svg()` element.
use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../assets/icons/", $name, ".svg")))),*
        ];
    };
}

icons!(
    "alert", "audio-lines", "copy", "back-5", "check", "chevron-down", "chevron-left", "chevron-right",
    "dot", "drive", "external", "film", "folder", "forward-5", "fullscreen", "gamepad", "grid",
    "keyboard", "mic", "mic-off", "minus", "more", "overlay", "pause", "pencil", "play", "plus",
    "power", "redo", "refresh", "repeat", "scissors", "search", "set-in", "set-out", "settings",
    "sliders", "square", "square-check", "star", "star-fill", "sun", "trash", "undo", "upload",
    "volume", "volume-x", "win-close", "win-max", "win-min", "win-restore", "x",
);

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS.iter().find(|(name, _)| *name == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
