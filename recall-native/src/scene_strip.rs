//! Scene-change scoring for the editor filmstrip.
//!
//! Every keyframe of the clip is sampled as a tiny 32x18 picture. Its score says how far it
//! is from the average of the samples just before it, in layout (per-pixel colour) and in
//! colour mix (a coarse colour histogram, which a camera pan barely moves). Both parts are
//! measured against the clip's own typical change, so 1.0 is an ordinary moment and a goal
//! explosion, a replay, a scoreboard or a menu stands out well above it.
//!
//! The strip keeps its tiles mapped to time: each tile shows the most remarkable frame of
//! its own stretch of the clip, or the one nearest its middle when nothing stands out.
use serde::{Deserialize, Serialize};

pub const SAMPLE_W: usize = 32;
pub const SAMPLE_H: usize = 18;
pub const SAMPLE_BYTES: usize = SAMPLE_W * SAMPLE_H * 3;
/// Samples (keyframes, ~1 s apart on recorder clips) the running average looks back over.
const WINDOW: usize = 8;
/// A score at or above this is an event worth showing over the middle of a stretch.
pub const REMARKABLE: f32 = 1.3;
/// Frames kept per clip; the view groups them into as many tiles as fit.
pub const MAX_FRAMES: usize = 64;

/// One kept filmstrip frame: a JPEG in the frame tier, its time and scene-change score.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StripFrame {
    pub path: String,
    pub seconds: f64,
    pub score: f32,
}

fn histogram(sample: &[u8]) -> [f32; 64] {
    let mut bins = [0f32; 64];
    for px in sample.chunks_exact(3) {
        bins[(px[0] as usize >> 6) * 16 + (px[1] as usize >> 6) * 4 + (px[2] as usize >> 6)] += 1.0;
    }
    let total = (sample.len() / 3).max(1) as f32;
    bins.iter_mut().for_each(|b| *b /= total);
    bins
}

fn median(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    sorted[sorted.len() / 2]
}

/// Scene-change score per sample (`SAMPLE_BYTES` of rgb24 each). The first is 0.
pub fn scores(samples: &[&[u8]]) -> Vec<f32> {
    let n = samples.len();
    let histograms: Vec<[f32; 64]> = samples.iter().map(|s| histogram(s)).collect();
    let mut layout = vec![0f32; n];
    let mut colour = vec![0f32; n];
    for i in 1..n {
        let lo = i.saturating_sub(WINDOW);
        let count = (i - lo) as f32;
        let mut diff = 0f32;
        for p in 0..SAMPLE_BYTES.min(samples[i].len()) {
            let mean = samples[lo..i].iter().map(|s| s.get(p).copied().unwrap_or(0) as f32).sum::<f32>() / count;
            diff += (samples[i][p] as f32 - mean).abs();
        }
        layout[i] = diff / SAMPLE_BYTES as f32;
        let mut hist = 0f32;
        for b in 0..64 {
            let mean = histograms[lo..i].iter().map(|h| h[b]).sum::<f32>() / count;
            hist += (histograms[i][b] - mean).abs();
        }
        colour[i] = 0.5 * hist;
    }
    if n < 2 {
        return vec![0.0; n];
    }
    // Floors keep a near-static clip (a paused menu) from turning noise into events.
    let typical_layout = median(&layout[1..]).max(2.0);
    let typical_colour = median(&colour[1..]).max(0.02);
    (0..n).map(|i| 0.5 * layout[i] / typical_layout + 0.5 * colour[i] / typical_colour).collect()
}

/// Of the samples in `members`, the most remarkable, or the one nearest `middle` when none is.
fn best_of(members: impl Iterator<Item = usize> + Clone, times: &[f64], scores: &[f32], middle: f64) -> Option<usize> {
    let top = members.clone().max_by(|a, b| scores[*a].total_cmp(&scores[*b]))?;
    if scores[top] >= REMARKABLE {
        return Some(top);
    }
    members.min_by(|a, b| (times[*a] - middle).abs().total_cmp(&(times[*b] - middle).abs()))
}

/// Which samples to keep (ascending): all of them when they fit, else one per equal stretch
/// of the clip, chosen by [`best_of`].
pub fn keep(times: &[f64], scores: &[f32], duration: f64, max: usize) -> Vec<usize> {
    if times.len() <= max {
        return (0..times.len()).collect();
    }
    let stretch = duration / max as f64;
    let mut kept = Vec::with_capacity(max);
    for k in 0..max {
        let (from, to) = (k as f64 * stretch, (k + 1) as f64 * stretch);
        let last = k + 1 == max;
        let members = (0..times.len()).filter(move |&i| times[i] >= from && (times[i] < to || last));
        if let Some(i) = best_of(members, times, scores, from + stretch / 2.0) {
            kept.push(i);
        }
    }
    kept
}

/// The frame shown in each tile of a strip `width` px wide whose tiles are `tile_w` px wide
/// (the last one may be cut off by the strip's edge). Tile `i` covers the clip time under it.
pub fn tile_picks(frames: &[StripFrame], duration: f64, width: f32, tile_w: f32) -> Vec<usize> {
    if frames.is_empty() || !(width > 0.0) || !(tile_w > 0.0) || !(duration > 0.0) {
        return Vec::new();
    }
    let tiles = (width / tile_w).ceil().clamp(1.0, 512.0) as usize;
    let per_px = duration / width as f64;
    let times: &[f64] = &frames.iter().map(|f| f.seconds).collect::<Vec<_>>();
    let scores: &[f32] = &frames.iter().map(|f| f.score).collect::<Vec<_>>();
    (0..tiles)
        .map(|t| {
            let from = t as f64 * tile_w as f64 * per_px;
            let to = (t + 1) as f64 * tile_w as f64 * per_px;
            let middle = (from + to.min(duration)) / 2.0;
            let members = (0..frames.len()).filter(move |&i| times[i] >= from && times[i] < to);
            best_of(members, times, scores, middle).unwrap_or_else(|| {
                (0..frames.len()).min_by(|a, b| (times[*a] - middle).abs().total_cmp(&(times[*b] - middle).abs())).unwrap_or(0)
            })
        })
        .collect()
}

/// A tile closer than this share of a tile to the next one would show as a sliver: of the two,
/// the more remarkable frame stays.
const MIN_VISIBLE: f32 = 0.3;

/// Left-anchored layout: `(frame, x)` per tile, drawn in order so a later tile covers the end of
/// the one before. A tile's left edge is the exact time of its frame, so clicking that edge lands
/// on it. The strip opens with the earliest frame at 0, and a frame repeats to fill a gap wider
/// than a tile until the next one starts.
pub fn anchored_tiles(frames: &[StripFrame], duration: f64, width: f32, tile_w: f32) -> Vec<(usize, f32)> {
    let mut picks = tile_picks(frames, duration, width, tile_w);
    picks.dedup();
    if picks.is_empty() {
        return Vec::new();
    }
    let x_of = |i: usize| ((frames[i].seconds / duration).clamp(0.0, 1.0) as f32) * width;
    let mut kept: Vec<usize> = Vec::with_capacity(picks.len());
    for i in picks {
        match kept.last().copied() {
            Some(last) if x_of(i) - x_of(last) < tile_w * MIN_VISIBLE => {
                if frames[i].score > frames[last].score {
                    *kept.last_mut().unwrap() = i;
                }
            }
            _ => kept.push(i),
        }
    }
    let mut tiles = Vec::new();
    if x_of(kept[0]) > 0.5 {
        tiles.push((0, 0.0));
        let mut x = tile_w;
        while x < x_of(kept[0]) - 0.5 {
            tiles.push((0, x));
            x += tile_w;
        }
    }
    for (n, &i) in kept.iter().enumerate() {
        let end = kept.get(n + 1).map_or(width, |&next| x_of(next));
        let mut x = x_of(i);
        loop {
            tiles.push((i, x));
            x += tile_w;
            if x >= end - 0.5 {
                break;
            }
        }
    }
    tiles
}

/// `pts_time` and the picture size from ffmpeg `showinfo` lines, one entry per frame.
pub fn parse_showinfo(log: &str) -> (Vec<f64>, Option<(u32, u32)>) {
    let mut times = Vec::new();
    let mut size = None;
    for line in log.lines().filter(|l| l.contains("Parsed_showinfo") && l.contains(" n:")) {
        let field = |key: &str| line.split_whitespace().find_map(|w| w.strip_prefix(key));
        let Some(time) = field("pts_time:").and_then(|v| v.parse::<f64>().ok()) else { continue };
        times.push(time);
        if size.is_none() {
            size = field("s:").and_then(|v| v.split_once('x')).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
        }
    }
    (times, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(r: u8, g: u8, b: u8) -> Vec<u8> {
        [r, g, b].repeat(SAMPLE_W * SAMPLE_H)
    }

    /// Gameplay-like noise: a busy picture that shifts a little every sample.
    fn busy(seed: usize) -> Vec<u8> {
        (0..SAMPLE_BYTES).map(|p| (((p * 7 + seed * 13) % 97) as u8).wrapping_add(60)).collect()
    }

    #[test]
    fn a_sudden_new_picture_scores_far_above_ordinary_play() {
        let mut samples: Vec<Vec<u8>> = (0..20).map(busy).collect();
        samples[12] = flat(250, 140, 20); // goal explosion
        let refs: Vec<&[u8]> = samples.iter().map(|s| s.as_slice()).collect();
        let s = scores(&refs);
        assert_eq!(s[0], 0.0);
        let others = s.iter().enumerate().filter(|(i, _)| *i != 12 && *i != 13 && *i > 0).map(|(_, v)| *v).fold(0f32, f32::max);
        assert!(s[12] > REMARKABLE && s[12] > 2.0 * others, "event {} vs ordinary max {others}", s[12]);
    }

    #[test]
    fn a_still_clip_has_no_events() {
        let samples: Vec<Vec<u8>> = (0..10).map(|_| flat(20, 20, 20)).collect();
        let refs: Vec<&[u8]> = samples.iter().map(|s| s.as_slice()).collect();
        assert!(scores(&refs).iter().all(|v| *v < REMARKABLE));
    }

    #[test]
    fn keep_takes_the_event_in_its_stretch_and_the_middle_elsewhere() {
        let times: Vec<f64> = (0..100).map(|i| i as f64).collect();
        let mut sc = vec![1.0f32; 100];
        sc[3] = 4.0;
        let kept = keep(&times, &sc, 100.0, 10);
        assert_eq!(kept.len(), 10);
        assert_eq!(kept[0], 3, "the event wins its stretch");
        assert_eq!(kept[1], 15, "a quiet stretch shows its middle");
        assert_eq!(keep(&times[..5], &sc[..5], 5.0, 10), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn tiles_fill_the_width_and_stay_in_time_order() {
        let frames: Vec<StripFrame> = (0..64).map(|i| StripFrame { path: format!("{i}"), seconds: i as f64 * 2.0 + 1.0, score: if i == 40 { 3.0 } else { 0.9 } }).collect();
        let picks = tile_picks(&frames, 128.0, 1000.0, 106.7);
        assert_eq!(picks.len(), 10, "ceil(1000 / 106.7): the last tile is cut by the edge");
        assert!(picks.windows(2).all(|w| w[0] < w[1]));
        let tile = (frames[40].seconds / 128.0 * 1000.0 / 106.7) as usize;
        assert_eq!(picks[tile], 40, "the event shows in the tile over its time");
    }

    #[test]
    fn more_tiles_than_frames_reuse_the_nearest() {
        let frames: Vec<StripFrame> = (0..3).map(|i| StripFrame { path: String::new(), seconds: i as f64 * 10.0 + 5.0, score: 0.0 }).collect();
        let picks = tile_picks(&frames, 30.0, 600.0, 100.0);
        assert_eq!(picks, vec![0, 0, 1, 1, 2, 2]);
    }

    #[test]
    fn tiles_start_exactly_at_their_frame_time() {
        let frames: Vec<StripFrame> = (0..64).map(|i| StripFrame { path: String::new(), seconds: i as f64 * 2.0, score: if i == 41 { 3.0 } else { 0.9 } }).collect();
        let (duration, width, tile_w) = (128.0, 1000.0, 106.7);
        let tiles = anchored_tiles(&frames, duration, width, tile_w);
        assert_eq!(tiles[0], (0, 0.0), "the strip opens at 0");
        assert!(tiles.windows(2).all(|w| w[0].1 < w[1].1), "left to right");
        let event = tiles.iter().find(|t| t.0 == 41).expect("the event is shown");
        assert!((event.1 - 82.0 / 128.0 * 1000.0).abs() < 0.01, "its left edge is its time");
        // No hole: each tile starts before the one before it ends, and the last reaches the edge.
        assert!(tiles.windows(2).all(|w| w[1].1 <= w[0].1 + tile_w + 0.01));
        assert!(tiles.last().unwrap().1 + tile_w >= width);
    }

    #[test]
    fn a_sliver_gives_way_to_the_more_remarkable_frame() {
        let frames = vec![
            StripFrame { path: String::new(), seconds: 0.0, score: 0.0 },
            StripFrame { path: String::new(), seconds: 9.0, score: 0.5 },
            StripFrame { path: String::new(), seconds: 10.0, score: 2.5 },
            StripFrame { path: String::new(), seconds: 19.0, score: 0.5 },
        ];
        let tiles = anchored_tiles(&frames, 20.0, 400.0, 100.0);
        assert!(tiles.iter().any(|t| t.0 == 2) && !tiles.iter().any(|t| t.0 == 1), "{tiles:?}");
    }

    #[test]
    fn showinfo_lines_give_times_and_size() {
        let log = "[Parsed_showinfo_0 @ 0x1] config in time_base: 1/1000000\n\
                   [Parsed_showinfo_0 @ 0x1] n:   0 pts:      0 pts_time:0       pos: 877 fmt:yuv420p sar:1/1 s:2560x1440 i:P iskey:1\n\
                   [Parsed_showinfo_0 @ 0x1] n:   1 pts: 997948 pts_time:0.997948 pos: 3419831 fmt:yuv420p sar:1/1 s:2560x1440 i:P\n";
        let (times, size) = parse_showinfo(log);
        assert_eq!(times, vec![0.0, 0.997948]);
        assert_eq!(size, Some((2560, 1440)));
    }
}
