//! Pure island layout: content → a scene of shapes and text runs in physical
//! pixels, measured through [`Measure`] (DirectWrite live, a fake in tests).
//! Every size below is in 1440p pixels and multiplied by the island scale.

use crate::notice::{NoticeView, SavePhase, Tone};
use crate::placement::ISLAND_H;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub rgb: u32,
    pub a: f32,
}

const fn rgba(rgb: u32, a: f32) -> Rgba {
    Rgba { rgb, a }
}

/// Recall theme tokens (recall-native `theme.rs`).
pub const PILL: Rgba = rgba(0x121212, 0.94);
pub const EDGE_LINE: Rgba = rgba(0xffffff, 0.07);
pub const TEXT: Rgba = rgba(0xededed, 1.0);
pub const MUTED: Rgba = rgba(0xa3a3a3, 1.0);
pub const TERTIARY: Rgba = rgba(0x8f8f8f, 1.0);
pub const SIGNAL: Rgba = rgba(0xff453a, 1.0);
pub const ACTIVE: Rgba = rgba(0x81c995, 1.0);
pub const WARNING: Rgba = rgba(0xfdd663, 1.0);
pub const ERROR: Rgba = rgba(0xf28b82, 1.0);
pub const TRACK: Rgba = rgba(0xffffff, 0.12);
/// A GPU at or above this temperature reads amber.
pub const HOT_GPU_C: u32 = 83;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    Geist500,
    Geist600,
    Bricolage700,
}

/// Text styles of the island.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    NoticeTitle,
    NoticeGame,
    NoticeUi,
    /// The shortcut hint's option rows.
    Chord,
    Replay,
    Fps,
    Small,
    RingNumber,
    Temp,
}

impl Role {
    /// Font size in 1440p pixels.
    pub fn size(self) -> f32 {
        match self {
            Role::NoticeTitle => 11.0,
            Role::NoticeGame => 15.0,
            Role::NoticeUi => 14.0,
            Role::Chord => 12.0,
            Role::Replay => 14.0,
            Role::Fps => 18.0,
            Role::Small => 9.5,
            Role::RingNumber => 10.5,
            Role::Temp => 11.0,
        }
    }

    pub fn face(self) -> Face {
        match self {
            Role::NoticeTitle | Role::Small | Role::Chord => Face::Geist500,
            Role::NoticeGame => Face::Bricolage700,
            _ => Face::Geist600,
        }
    }
}

/// A laid-out line: advance width, box height, baseline and cap height from
/// the box top (physical px).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextMetrics {
    pub width: f32,
    pub height: f32,
    pub baseline: f32,
    pub cap: f32,
}

pub trait Measure {
    fn measure(&self, role: Role, scale: f32, text: &str) -> TextMetrics;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    pub role: Role,
    pub text: String,
    /// Layout box top-left.
    pub x: f32,
    pub y: f32,
    pub color: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Dot { cx: f32, cy: f32, r: f32, color: Rgba },
    Hollow { cx: f32, cy: f32, r: f32, stroke: f32, color: Rgba },
    Pause { x: f32, cy: f32, bar_w: f32, bar_h: f32, gap: f32, color: Rgba },
    Ring { cx: f32, cy: f32, r: f32, stroke: f32, frac: f32, color: Rgba },
}

/// The island's leading mark (tone dot, pause bars, hollow ring, or a clip
/// save's shutter ring), kept apart from the rest of the scene: it rides
/// the pill's appear / hide circle and runs the save animation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Icon {
    pub shape: Shape,
    pub cx: f32,
    pub cy: f32,
    /// The mark's dot radius (the shutter ring is twice that).
    pub r: f32,
    /// A clip save: draw the shutter-ring badge here instead of `shape`.
    pub badge: Option<SavePhase>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub w: f32,
    pub h: f32,
    pub icon: Option<Icon>,
    pub shapes: Vec<Shape>,
    pub texts: Vec<Text>,
}

/// The stats island's numbers, ready to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsContent {
    pub recording: bool,
    pub paused: bool,
    pub replay_secs: u64,
    pub fps: Option<u32>,
    pub low1: Option<u32>,
    /// Shown under the FPS when there is no framerate ("No frame timing").
    pub fps_note: Option<String>,
    pub gpu: Option<u32>,
    pub gpu_temp: Option<u32>,
    pub cpu: Option<u32>,
}

struct Pen<'a> {
    s: f32,
    m: &'a dyn Measure,
    scene: Scene,
}

impl Pen<'_> {
    fn u(&self, v: f32) -> f32 {
        v * self.s
    }

    fn width(&self, role: Role, texts: &[&str]) -> f32 {
        texts.iter().map(|t| self.m.measure(role, self.s, t).width).fold(0.0, f32::max)
    }

    /// Places `text` so its cap height is centred on `cy` (or its baseline at `baseline`).
    fn text_at_baseline(&mut self, role: Role, text: &str, x: f32, baseline: f32, color: Rgba) {
        let metrics = self.m.measure(role, self.s, text);
        let y = baseline - metrics.baseline;
        self.scene.texts.push(Text { role, text: text.into(), x, y, color });
    }

    fn cap(&self, role: Role, text: &str) -> f32 {
        self.m.measure(role, self.s, text).cap
    }

    /// Baselines for two stacked lines whose caps are centred together.
    fn two_lines(&self, top: (Role, &str), bottom: (Role, &str), gap: f32) -> (f32, f32) {
        let (c1, c2) = (self.cap(top.0, top.1), self.cap(bottom.0, bottom.1));
        let first = (self.scene.h - (c1 + gap + c2)) / 2.0 + c1;
        (first, first + gap + c2)
    }

    fn centred_baseline(&self, role: Role, text: &str) -> f32 {
        (self.scene.h + self.cap(role, text)) / 2.0
    }

    fn mark(&mut self, tone: Tone, x: f32, badge: Option<SavePhase>) {
        let (cy, r, color) = (self.scene.h / 2.0, self.u(4.0), tone_color(tone));
        let shape = match tone {
            Tone::Waiting => Shape::Hollow { cx: x + r, cy, r, stroke: self.u(1.5), color },
            Tone::Paused => Shape::Pause {
                x: x + self.u(0.5),
                cy,
                bar_w: self.u(2.5),
                bar_h: self.u(9.0),
                gap: self.u(2.0),
                color,
            },
            _ => Shape::Dot { cx: x + r, cy, r, color },
        };
        self.scene.icon = Some(Icon { shape, cx: x + r, cy, r, badge });
    }
}

/// The colour of a tone's mark (the action notice's sweep reuses it).
pub fn tone_color(tone: Tone) -> Rgba {
    match tone {
        Tone::Recording => SIGNAL,
        Tone::Success => ACTIVE,
        Tone::Failure => ERROR,
        Tone::Neutral | Tone::Waiting => TERTIARY,
        Tone::Paused => MUTED,
    }
}

fn pen(s: f32, m: &dyn Measure) -> Pen<'_> {
    Pen { s, m, scene: Scene { h: ISLAND_H * s, ..Scene::default() } }
}

/// Mark + small title over a bold detail (or a single bold line).
pub fn notice_scene(notice: &NoticeView, s: f32, m: &dyn Measure) -> Scene {
    let mut p = pen(s, m);
    let (pad_l, mark_w, gap, pad_r) = (p.u(16.0), p.u(8.0), p.u(10.0), p.u(20.0));
    p.mark(notice.tone, pad_l, notice.save);
    // The shutter ring is twice the dot: its text steps right to keep the gap.
    let x = pad_l + mark_w + gap + if notice.save.is_some() { p.u(4.0) } else { 0.0 };
    let text_w = if notice.chord {
        let (b1, b2) = p.two_lines((Role::Chord, &notice.title), (Role::Chord, &notice.detail), p.u(6.0));
        p.text_at_baseline(Role::Chord, &notice.title, x, b1, TEXT);
        p.text_at_baseline(Role::Chord, &notice.detail, x, b2, TEXT);
        p.width(Role::Chord, &[&notice.title, &notice.detail])
    } else if notice.detail.is_empty() {
        let baseline = p.centred_baseline(Role::NoticeUi, &notice.title);
        p.text_at_baseline(Role::NoticeUi, &notice.title, x, baseline, TEXT);
        p.width(Role::NoticeUi, &[&notice.title])
    } else {
        let detail_role = if notice.game_font { Role::NoticeGame } else { Role::NoticeUi };
        let (b1, b2) = p.two_lines((Role::NoticeTitle, &notice.title), (detail_role, &notice.detail), p.u(5.5));
        p.text_at_baseline(Role::NoticeTitle, &notice.title, x, b1, MUTED);
        p.text_at_baseline(detail_role, &notice.detail, x, b2, TEXT);
        p.width(Role::NoticeTitle, &[&notice.title]).max(p.width(detail_role, &[&notice.detail]))
    };
    p.scene.w = (x + text_w + pad_r).max(p.scene.h * 2.0).ceil();
    p.scene
}

/// "m:ss" (or "h:mm:ss") for the replay length.
pub fn clock(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// Minimal style: rec mark + replay length · FPS over "1% low" · GPU ring
/// (+ temperature) · CPU ring. Slots reserve room for 3-digit values so the
/// pill does not twitch as numbers change.
pub fn stats_scene(st: &StatsContent, s: f32, m: &dyn Measure) -> Scene {
    let mut p = pen(s, m);
    let group_gap = p.u(16.0);
    let mut x = p.u(16.0);
    let tone = if st.paused { Tone::Paused } else if st.recording { Tone::Recording } else { Tone::Waiting };
    p.mark(tone, x, None);
    x += p.u(8.0) + p.u(7.0);

    let replay = clock(st.replay_secs);
    let baseline = p.centred_baseline(Role::Replay, "0");
    p.text_at_baseline(Role::Replay, &replay, x, baseline, if st.recording { TEXT } else { MUTED });
    x += p.width(Role::Replay, &[&replay, "0:00"]) + group_gap;

    let fps = st.fps.map_or("—".to_string(), |f| f.to_string());
    let low = match (st.fps, st.low1, &st.fps_note) {
        (None, _, Some(note)) => note.clone(),
        (_, Some(low), _) => format!("1% low {low}"),
        _ => "1% low —".to_string(),
    };
    let (b1, b2) = p.two_lines((Role::Fps, "0"), (Role::Small, "1"), p.u(4.5));
    p.text_at_baseline(Role::Fps, &fps, x, b1, TEXT);
    p.text_at_baseline(Role::Small, &low, x, b2, TERTIARY);
    x += p.width(Role::Fps, &[&fps, "000"]).max(p.width(Role::Small, &[&low, "1% low 000"])) + group_gap;

    if let Some(gpu) = st.gpu {
        let hot = st.gpu_temp.is_some_and(|t| t >= HOT_GPU_C);
        x = ring(&mut p, x, Some(gpu), if hot { WARNING } else { TEXT });
        let temp = st.gpu_temp.map(|t| format!("{t}°C"));
        let col = p.width(Role::Small, &["GPU"]).max(p.width(Role::Temp, &[temp.as_deref().unwrap_or(""), "00°C"]));
        match &temp {
            Some(temp) => {
                let (b1, b2) = p.two_lines((Role::Small, "G"), (Role::Temp, "0"), p.u(4.0));
                p.text_at_baseline(Role::Small, "GPU", x, b1, TERTIARY);
                p.text_at_baseline(Role::Temp, temp, x, b2, if hot { WARNING } else { TEXT });
            }
            None => {
                let baseline = p.centred_baseline(Role::Small, "G");
                p.text_at_baseline(Role::Small, "GPU", x, baseline, TERTIARY);
            }
        }
        x += col + group_gap;
    }

    x = ring(&mut p, x, st.cpu, TEXT);
    let baseline = p.centred_baseline(Role::Small, "C");
    p.text_at_baseline(Role::Small, "CPU", x, baseline, TERTIARY);
    x += p.width(Role::Small, &["CPU"]) + p.u(18.0);
    p.scene.w = x.ceil();
    p.scene
}

/// A usage ring at `x` with its number inside (a dash before the first
/// reading); returns the x after it.
fn ring(p: &mut Pen, x: f32, value: Option<u32>, color: Rgba) -> f32 {
    let (d, stroke) = (p.u(30.0), p.u(2.75));
    let (cx, cy) = (x + d / 2.0, p.scene.h / 2.0);
    let value = value.map(|v| v.min(100));
    let frac = value.unwrap_or(0) as f32 / 100.0;
    p.scene.shapes.push(Shape::Ring { cx, cy, r: (d - stroke) / 2.0, stroke, frac, color });
    let number = value.map_or("—".to_string(), |v| v.to_string());
    let metrics = p.m.measure(Role::RingNumber, p.s, &number);
    let baseline = cy + metrics.cap / 2.0;
    p.text_at_baseline(Role::RingNumber, &number, cx - metrics.width / 2.0, baseline, TEXT);
    x + d + p.u(6.0)
}

/// How a usage ring's value arc is drawn: clockwise from 12 o'clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sweep {
    Empty,
    Full,
    Arc { start: (f32, f32), end: (f32, f32), large: bool },
}

pub fn sweep(cx: f32, cy: f32, r: f32, frac: f32) -> Sweep {
    if frac <= 0.005 {
        return Sweep::Empty;
    }
    if frac >= 0.995 {
        return Sweep::Full;
    }
    let angle = frac * std::f32::consts::TAU;
    Sweep::Arc { start: (cx, cy - r), end: (cx + r * angle.sin(), cy - r * angle.cos()), large: frac > 0.5 }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
