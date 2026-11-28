//! What the Record picker offers (`views::record_picker`): open app windows and monitors,
//! and the plain rules around them: card labels, the monitor layout a screen card draws, a
//! thumbnail's fit inside its card, and which card starts selected. The Windows reads are in
//! `capture_targets_win`; the live window previews in `dwm_thumbs`.
use serde::{Deserialize, Serialize};

/// An app window the picker offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowCard {
    pub hwnd: isize,
    pub pid: u32,
    /// The window's title (one line).
    pub title: String,
    /// `game.exe`.
    pub exe: String,
    /// Full exe path, for its icon ("" when unknown).
    pub path: String,
}

impl WindowCard {
    /// The name under the card: the title, else the exe without `.exe`.
    pub fn label(&self) -> String {
        let title = crate::services::audio_apps::clip_label(&self.title);
        if title.is_empty() { crate::services::audio_apps::exe_stem(&self.exe) } else { title }
    }
}

/// A monitor the picker offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MonitorCard {
    /// GDI device name, `\\.\DISPLAY2`.
    pub device: String,
    /// Desktop rect in physical px: left, top, right, bottom.
    pub rect: [i32; 4],
    pub primary: bool,
}

impl MonitorCard {
    pub fn size(&self) -> (i32, i32) {
        (self.rect[2] - self.rect[0], self.rect[3] - self.rect[1])
    }

    /// "Display 2", from the device name's number (Windows' own display numbering).
    pub fn name(&self) -> String {
        let digits: String = self.device.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
        if digits.is_empty() { "Display".into() } else { format!("Display {digits}") }
    }

    /// "2560 × 1440 · Main".
    pub fn detail(&self) -> String {
        let (w, h) = self.size();
        if self.primary { format!("{w} × {h} · Main") } else { format!("{w} × {h}") }
    }
}

/// The picker's two tabs (maintainer, 2026-11-28: no Devices tab).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Applications,
    Screens,
}

/// What the picker sends (`record_pick`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Window(isize),
    Monitor(String),
}

/// The last pick, kept across starts to preselect (maintainer: "yep"). Per PC: it names this PC's
/// exe or monitor, so it lives in the app's data folder, never in the repo.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastPick {
    /// `window` | `monitor`.
    pub kind: String,
    /// The window's exe, or the monitor's device name.
    pub key: String,
}

impl LastPick {
    pub fn of(choice: &Choice, windows: &[WindowCard]) -> Option<Self> {
        match choice {
            Choice::Window(hwnd) => windows.iter().find(|w| w.hwnd == *hwnd).map(|w| LastPick { kind: "window".into(), key: w.exe.clone() }),
            Choice::Monitor(device) => Some(LastPick { kind: "monitor".into(), key: device.clone() }),
        }
    }
}

/// The tab and card selected when the picker opens: the followed game's window, else the
/// last pick (its exe's front window, or its monitor), else the main monitor.
pub fn preselect(windows: &[WindowCard], monitors: &[MonitorCard], last: Option<&LastPick>, followed: Option<isize>) -> (Tab, Option<Choice>) {
    if let Some(w) = followed.and_then(|h| windows.iter().find(|w| w.hwnd == h)) {
        return (Tab::Applications, Some(Choice::Window(w.hwnd)));
    }
    if let Some(last) = last {
        if last.kind == "window" {
            if let Some(w) = windows.iter().find(|w| w.exe.eq_ignore_ascii_case(&last.key)) {
                return (Tab::Applications, Some(Choice::Window(w.hwnd)));
            }
        } else if let Some(m) = monitors.iter().find(|m| m.device.eq_ignore_ascii_case(&last.key)) {
            return (Tab::Screens, Some(Choice::Monitor(m.device.clone())));
        }
    }
    let main = monitors.iter().find(|m| m.primary).or(monitors.first());
    (Tab::Screens, main.map(|m| Choice::Monitor(m.device.clone())))
}

/// The followed game's window first, the rest in z-order (front first).
pub fn order_windows(mut windows: Vec<WindowCard>, followed: Option<isize>) -> Vec<WindowCard> {
    if let Some(i) = followed.and_then(|h| windows.iter().position(|w| w.hwnd == h)) {
        let w = windows.remove(i);
        windows.insert(0, w);
    }
    windows
}

/// A rect as left, top, right, bottom.
pub type Rect = [f32; 4];

/// Each monitor's rect inside a `w` x `h` box (`pad` on every side), the whole desktop scaled
/// to fit and centred: what a screen card draws, so a rotated monitor reads as one.
pub fn layout_rects(monitors: &[MonitorCard], w: f32, h: f32, pad: f32) -> Vec<Rect> {
    let Some(first) = monitors.first() else { return Vec::new() };
    let mut union = first.rect;
    for m in monitors {
        union = [union[0].min(m.rect[0]), union[1].min(m.rect[1]), union[2].max(m.rect[2]), union[3].max(m.rect[3])];
    }
    let (uw, uh) = ((union[2] - union[0]).max(1) as f32, (union[3] - union[1]).max(1) as f32);
    let scale = ((w - 2. * pad).max(1.) / uw).min((h - 2. * pad).max(1.) / uh);
    let (ox, oy) = ((w - uw * scale) / 2., (h - uh * scale) / 2.);
    monitors
        .iter()
        .map(|m| {
            let x = |v: i32| ox + (v - union[0]) as f32 * scale;
            let y = |v: i32| oy + (v - union[1]) as f32 * scale;
            [x(m.rect[0]), y(m.rect[1]), x(m.rect[2]), y(m.rect[3])]
        })
        .collect()
}

/// A `src_w` x `src_h` picture fit inside `dest` (aspect kept, centred).
pub fn fit(src_w: i32, src_h: i32, dest: [i32; 4]) -> [i32; 4] {
    let (dw, dh) = ((dest[2] - dest[0]).max(0), (dest[3] - dest[1]).max(0));
    if src_w <= 0 || src_h <= 0 || dw == 0 || dh == 0 {
        return dest;
    }
    let scale = (dw as f64 / src_w as f64).min(dh as f64 / src_h as f64);
    let (w, h) = ((src_w as f64 * scale).round() as i32, (src_h as f64 * scale).round() as i32);
    let (x, y) = (dest[0] + (dw - w) / 2, dest[1] + (dh - h) / 2);
    [x, y, x + w, y + h]
}

/// `dest` (a picture of a `src_w` x `src_h` source) cut to `clip`: the visible part and the
/// source rect that maps onto it; `None` when nothing shows (a card scrolled out of view).
pub fn clip(dest: [i32; 4], src_w: i32, src_h: i32, clip: [i32; 4]) -> Option<([i32; 4], [i32; 4])> {
    let shown = [dest[0].max(clip[0]), dest[1].max(clip[1]), dest[2].min(clip[2]), dest[3].min(clip[3])];
    if shown[2] <= shown[0] || shown[3] <= shown[1] {
        return None;
    }
    let (dw, dh) = ((dest[2] - dest[0]).max(1) as f64, (dest[3] - dest[1]).max(1) as f64);
    let sx = |v: i32| ((v - dest[0]) as f64 / dw * src_w as f64).round() as i32;
    let sy = |v: i32| ((v - dest[1]) as f64 / dh * src_h as f64).round() as i32;
    Some((shown, [sx(shown[0]), sy(shown[1]), sx(shown[2]), sy(shown[3])]))
}

/// The clip line at the bottom: what size and rate the recording takes, from Settings.
/// `height` 0 = the picked window's or monitor's own size (the resolution rule).
pub fn clip_line(height: u64, fps: u64) -> String {
    let size = if height == 0 { "Size follows what you pick".to_string() } else { format!("{height}p") };
    format!("{size} · {fps} fps")
}

#[cfg(test)]
#[path = "capture_targets_tests.rs"]
mod tests;
