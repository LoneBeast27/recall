//! The comparison images must show what each tab names: one recording per setting, all of
//! the same frame, and the setup page's full frames must be that same frame.
use std::collections::HashSet;

use super::*;

fn rgb(bytes: &[u8]) -> image::RgbImage {
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png).expect("decodes").to_rgb8()
}

fn mean_abs_diff(a: &[f64], b: &[f64]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f64>() / a.len() as f64
}

fn all_recipes() -> impl Iterator<Item = Recipe> {
    (0..CODECS.len()).flat_map(|codec| (0..BITRATES.len()).flat_map(move |bitrate| (0..EFFORT.len()).map(move |effort| Recipe { codec, bitrate, effort })))
}

fn bytes(r: Recipe) -> &'static [u8] {
    SAMPLES[r.codec][r.bitrate][r.effort]
}

#[test]
fn sample_names_follow_the_settings_orders() {
    // `SAMPLES` is built from these literal names, so the Settings orders must match them.
    assert_eq!(CODECS.map(|(v, _)| v), ["h264", "hevc", "av1"]);
    assert_eq!(BITRATES, [15, 25, 40, 60]);
    assert_eq!(EFFORT.map(|(v, _)| v), ["performance", "balanced", "quality"]);
}

#[test]
fn every_option_has_its_own_recording() {
    let mut seen = HashSet::new();
    assert!(seen.insert(ORIGINAL));
    for r in all_recipes() {
        assert!(seen.insert(bytes(r)), "{} reuses another tab's image", r.label());
    }
}

#[test]
fn every_crop_is_the_same_frame_as_the_original() {
    let original: Vec<f64> = rgb(ORIGINAL).as_raw().iter().map(|&v| v as f64).collect();
    assert_eq!(rgb(ORIGINAL).dimensions(), (TILE as u32, TILE as u32));
    for r in all_recipes() {
        let img = rgb(bytes(r));
        assert_eq!(img.dimensions(), (TILE as u32, TILE as u32), "{}", r.label());
        let px: Vec<f64> = img.as_raw().iter().map(|&v| v as f64).collect();
        // Encoding loss at 15 Mbps is ~15; another picture is several times that.
        let diff = mean_abs_diff(&px, &original);
        assert!(diff < 30., "{} differs from the original by {diff:.1}", r.label());
    }
}

/// Where the crop sits in the 2560x1440 frame, as `make-previews.ps1` wrote it.
fn crop_spot() -> (u32, u32, u32) {
    let made: serde_json::Value = serde_json::from_str(include_str!("../../assets/quality/previews.json")).expect("previews.json");
    let n = |k: &str| made[k].as_u64().unwrap_or_else(|| panic!("previews.json: {k}")) as u32;
    (n("crop_x"), n("crop_y"), n("size"))
}

/// The crop's area in a 640x360 full frame, from the pixel-doubled crop: undo the doubling,
/// then average 4x4 blocks (the frame is the 2560x1440 one scaled by 1/4).
fn crop_at_frame_scale(crop: &image::RgbImage, size: u32) -> Vec<f64> {
    let s = size / 4;
    let mut out = Vec::with_capacity((s * s * 3) as usize);
    for y in 0..s {
        for x in 0..s {
            for ch in 0..3 {
                let mut sum = 0.;
                for dy in 0..4 {
                    for dx in 0..4 {
                        sum += crop.get_pixel((x * 4 + dx) * 2, (y * 4 + dy) * 2)[ch] as f64;
                    }
                }
                out.push(sum / 16.);
            }
        }
    }
    out
}

fn frame_area(frame: &image::RgbImage, x: u32, y: u32, size: u32) -> Vec<f64> {
    let (x, y, s) = (x / 4, y / 4, size / 4);
    let mut out = Vec::with_capacity((s * s * 3) as usize);
    for fy in y..y + s {
        for fx in x..x + s {
            out.extend(frame.get_pixel(fx, fy).0.iter().map(|&v| v as f64));
        }
    }
    out
}

#[test]
fn setup_full_frames_are_the_frame_the_crops_come_from() {
    let (x, y, size) = crop_spot();
    assert!(x % 4 == 0 && y % 4 == 0 && size % 4 == 0, "crop spot must sit on the 1/4 grid");
    let pairs = std::iter::once((ORIGINAL_FRAME, ORIGINAL, "original".to_owned()))
        .chain((0..BITRATES.len()).map(|b| (FRAMES[b], bytes(Recipe { codec: 0, bitrate: b, effort: 1 }), format!("{} Mbps", BITRATES[b]))));
    for (frame, crop, name) in pairs {
        let frame = rgb(frame);
        assert_eq!(frame.dimensions(), (640, 360), "{name}");
        // Same spot of the same recording: a few levels apart (scaling filters differ).
        let diff = mean_abs_diff(&crop_at_frame_scale(&rgb(crop), size), &frame_area(&frame, x, y, size));
        assert!(diff < 8., "{name}: full frame and crop differ by {diff:.1}");
    }
}

#[test]
fn a_tab_changes_only_its_own_setting() {
    let now = Recipe { codec: 2, bitrate: 1, effort: 2 };
    assert_eq!(now.with(Preview::Bitrate, 3), Recipe { codec: 2, bitrate: 3, effort: 2 });
    assert_eq!(now.with(Preview::Effort, 0), Recipe { codec: 2, bitrate: 1, effort: 0 });
    assert_eq!(now.option(Preview::Bitrate), 1);
    assert_eq!(now.option(Preview::Effort), 2);
}

#[test]
fn current_settings_map_to_their_recording() {
    assert_eq!(Recipe::current("hevc", 40, "quality"), Recipe { codec: 1, bitrate: 2, effort: 2 });
    assert_eq!(Recipe::current("av1", 60, "performance"), Recipe { codec: 2, bitrate: 3, effort: 0 });
    // Off-list values: nearest bitrate, H.264 / Balanced.
    assert_eq!(Recipe::current("vp9", 100, "turbo"), Recipe { codec: 0, bitrate: 3, effort: 1 });
    assert_eq!(nearest_bitrate(30), 1);
    assert_eq!(nearest_bitrate(1), 0);
}
