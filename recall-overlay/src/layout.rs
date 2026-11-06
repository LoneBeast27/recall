//! Pure island layout: content → a scene of shapes and text runs in physical
//! pixels, measured through [`Measure`] (DirectWrite live, a fake in tests).
//! Every size below is in 1440p pixels and multiplied by the island scale.

use crate::cells::{count, Cells, Fill};
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
    /// A shortcut hint key cap's key ("1") and the leader pill ("Alt `").
    Key,
    /// A shortcut hint option's label ("Bookmark").
    Option,
    /// A stats number cell (a replay timer part, fps, 1% low, temperature):
    /// 3 tabular digits fit a cell with [`crate::cells::MARGIN`] either side.
    Fps,
    /// A notice's number cell (a timer part in the detail): the size of
    /// [`Role::Fps`], fading with the detail line.
    NoticeNumber,
    Small,
    RingNumber,
}

impl Role {
    /// Font size in 1440p pixels.
    pub fn size(self) -> f32 {
        match self {
            Role::NoticeTitle => 12.0,
            Role::NoticeGame => 15.0,
            Role::NoticeUi => 14.0,
            Role::Key => 12.0,
            Role::Option => 13.0,
            Role::Fps | Role::NoticeNumber => 16.0,
            Role::Small => 10.0,
            Role::RingNumber => 10.5,
        }
    }

    pub fn face(self) -> Face {
        match self {
            Role::NoticeTitle | Role::Small | Role::Option => Face::Geist500,
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
    /// The content starts at the pill's left padding instead of centring: a
    /// save's stages keep the mark and the lines where they are while the
    /// pill changes width.
    pub lead: bool,
    /// Re-flowed for a side spot ([`crate::stack`]): each text is one upright
    /// cell's content (or a ring's number) centred on its `x`, `y`.
    pub stacked: bool,
    /// Laid out on the square cell grid ([`crate::cells`]): how many cells,
    /// and the pitch from one cell's start to the next (0: not a grid).
    pub cells: usize,
    pub pitch: f32,
    /// A horizontal pill that draws prose whole: its cells, which the side
    /// pill stacks instead ([`crate::stack`]).
    pub tall: Option<Box<Scene>>,
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

    fn centred_baseline(&self, role: Role, text: &str) -> f32 {
        (self.scene.h + self.m.measure(role, self.s, text).cap) / 2.0
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

/// A clip save's titles, stage by stage.
pub const SAVE_TITLES: [&str; 3] = ["Saving", "Clip saved", "Save failed"];

/// Mark, then the title, then the detail ([`crate::cells`]): compact
/// fields in square cells, prose as continuous runs on a horizontal pill.
pub fn notice_scene(notice: &NoticeView, s: f32, m: &dyn Measure) -> Scene {
    let mut cells = Cells::new(s, m);
    cells.push(Fill::Mark(notice.tone, notice.save));
    if notice.detail.is_empty() {
        cells.text(&notice.title, Role::NoticeUi, Role::NoticeNumber, TEXT, 0.0);
    } else {
        // A save's title changes word by stage: room for the longest, so the
        // pill and the detail never move when the clip lands.
        let hold = notice.save.is_some();
        let widest = if hold { SAVE_TITLES.iter().map(|t| m.measure(Role::NoticeTitle, s, t).width).fold(0.0, f32::max) } else { 0.0 };
        cells.text(&notice.title, Role::NoticeTitle, Role::NoticeTitle, MUTED, widest);
        if hold {
            let most = SAVE_TITLES.iter().map(|t| count(t, Role::NoticeTitle, Role::NoticeTitle, s, m)).max().unwrap_or(0);
            let title = count(&notice.title, Role::NoticeTitle, Role::NoticeTitle, s, m);
            (title..most).for_each(|_| cells.push(Fill::Blank));
            // The held cells belong to the title's run on a horizontal pill.
            let end = cells.out.len();
            if let Some(span) = cells.spans.last_mut() {
                span.end = end;
            }
        }
        let detail_role = if notice.game_font { Role::NoticeGame } else { Role::NoticeUi };
        cells.text(&notice.detail, detail_role, Role::NoticeNumber, TEXT, 0.0);
    }
    let mut scene = cells.scene();
    scene.lead = notice.save.is_some();
    if let Some(tall) = scene.tall.as_mut() {
        tall.lead = scene.lead;
    }
    scene
}

/// The island's small pill while the shortcut hint is open and nothing else
/// shows: the leader keys ("Alt `"), centred.
pub fn leader_scene(leader: &str, s: f32, m: &dyn Measure) -> Scene {
    let mut p = pen(s, m);
    let (pad, width) = (p.u(18.0), p.width(Role::Key, &[leader]));
    let baseline = p.centred_baseline(Role::Key, leader);
    p.scene.w = (width + pad * 2.0).max(p.scene.h * 2.0).ceil();
    let x = (p.scene.w - width) / 2.0;
    p.text_at_baseline(Role::Key, leader, x, baseline, MUTED);
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

/// Minimal style, one cell each: rec mark · replay minutes | seconds · fps
/// · "1% low" · its number · GPU ring · "GPU" · temperature · CPU ring ·
/// "CPU". The cell count holds still as numbers change (3 digits a cell).
pub fn stats_scene(st: &StatsContent, s: f32, m: &dyn Measure) -> Scene {
    let mut cells = Cells::new(s, m);
    let tone = if st.paused { Tone::Paused } else if st.recording { Tone::Recording } else { Tone::Waiting };
    cells.push(Fill::Mark(tone, None));
    cells.run(&clock(st.replay_secs), Role::Small, Role::Fps, if st.recording { TEXT } else { MUTED });
    let dash = |cells: &mut Cells, role, color| cells.push(Fill::Text(role, "—".into(), color));
    match st.fps {
        Some(fps) => cells.run(&fps.to_string(), Role::Small, Role::Fps, TEXT),
        None => dash(&mut cells, Role::Fps, TEXT),
    }
    match (st.fps, st.low1, &st.fps_note) {
        (None, _, Some(note)) => cells.text(note, Role::Small, Role::Small, TERTIARY, 0.0),
        (_, low, _) => {
            cells.push(Fill::Pair(Role::Small, ["1%", "low"], TERTIARY));
            match low {
                Some(low) => cells.run(&low.to_string(), Role::Small, Role::Fps, MUTED),
                None => dash(&mut cells, Role::Fps, MUTED),
            }
        }
    }
    if let Some(gpu) = st.gpu {
        let hot = st.gpu_temp.is_some_and(|t| t >= HOT_GPU_C);
        cells.push(Fill::Ring(Some(gpu), if hot { WARNING } else { TEXT }));
        cells.push(Fill::Text(Role::Small, "GPU".into(), TERTIARY));
        if let Some(temp) = st.gpu_temp {
            cells.run(&format!("{temp}°"), Role::Small, Role::Fps, if hot { WARNING } else { TEXT });
        }
    }
    cells.push(Fill::Ring(st.cpu, TEXT));
    cells.push(Fill::Text(Role::Small, "CPU".into(), TERTIARY));
    let mut scene = cells.scene();
    scene.lead = true; // the record dot stays put through a transformation
    if let Some(tall) = scene.tall.as_mut() {
        tall.lead = true;
    }
    scene
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
