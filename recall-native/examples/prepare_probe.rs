//! Exercise the native editor's actual read-only media preparation and visual cache.
#[path = "../src/build_flavor.rs"]
mod build_flavor;
#[allow(dead_code)]
#[path = "../src/user_dirs.rs"]
mod user_dirs;
pub(crate) use build_flavor::TESTING;
#[path = "../../recall/src/edit_model.rs"]
mod edit_model;
#[path = "../../recall/src/clip_rate.rs"]
mod clip_rate;
#[allow(dead_code)]
#[path = "../../recall/src/win_recycle.rs"]
mod win_recycle;
#[allow(dead_code)]
#[path = "../../recall/src/win_fileop.rs"]
mod win_fileop;
#[allow(dead_code)]
#[path = "../../recall/src/win_volume.rs"]
mod win_volume;
#[allow(dead_code)]
#[path = "../../recall/src/file_id.rs"]
mod file_id;
#[allow(dead_code)]
#[path = "../../recall/src/same_bytes.rs"]
mod same_bytes;
#[path = "../src/backend.rs"]
mod backend;
#[path = "../src/services/mod.rs"]
#[allow(dead_code)]
mod services;
#[path = "../src/startup.rs"]
#[allow(dead_code)]
mod startup;
use std::time::Instant;

fn main() {
    let source = std::env::args().nth(1).expect("pass a clip path");
    if source == "--thumbnail" {
        let path = std::env::args().nth(2).expect("pass a clip path");
        let started = Instant::now();
        let thumbnail = backend::thumbnail(&path).expect("generate actual library thumbnail");
        let first_ms = started.elapsed().as_millis();
        let modified = std::fs::metadata(&thumbnail).unwrap().modified().unwrap();
        let cached = Instant::now();
        assert_eq!(backend::thumbnail(&path).unwrap(), thumbnail);
        let cached_ms = cached.elapsed().as_millis();
        assert_eq!(std::fs::metadata(&thumbnail).unwrap().modified().unwrap(), modified);
        let clips = backend::list_clips().unwrap();
        let entry = clips.iter().find(|entry| std::fs::canonicalize(&entry.path).unwrap() == std::fs::canonicalize(&path).unwrap()).expect("test clip in library");
        assert_eq!(entry.thumb.as_deref(), Some(thumbnail.as_str()), "listing discovers the same lazy cache file");
        println!("{}", serde_json::json!({"verdict":"PASS","thumbnail":thumbnail,"first_ms":first_ms,"cached_ms":cached_ms,"listing_reused_cache":true}));
        return;
    }
    if source == "--list" {
        let started = Instant::now();
        let clips = backend::list_clips().expect("list actual library");
        println!("{}", serde_json::json!({
            "verdict":"PASS","clips":clips.len(),"elapsed_ms":started.elapsed().as_millis(),
            "with_thumbnails":clips.iter().filter(|clip|clip.thumb.is_some()).count(),
            "first":clips.iter().take(3).collect::<Vec<_>>()
        }));
        return;
    }
    let first_start = Instant::now();
    let first = backend::prepare_clip(&source).expect("prepare source");
    let first_ms = first_start.elapsed().as_millis();
    let paths: Vec<_> = first.visuals.poster.iter().chain(first.visuals.filmstrip.iter().map(|f| &f.path)).collect();
    assert!(!first.visuals.filmstrip.is_empty(), "real filmstrip frames");
    assert!(first.visuals.filmstrip.windows(2).all(|w| w[0].seconds <= w[1].seconds), "filmstrip in time order");
    assert!(first.visuals.aspect > 0.0, "picture aspect known");
    let stamps: Vec<_> = paths.iter().map(|path| {
        let bytes = std::fs::read(path).expect("read generated image");
        assert!(bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9]), "valid JPEG marker envelope");
        std::fs::metadata(path).unwrap().modified().unwrap()
    }).collect();
    let cached_start = Instant::now();
    let second = backend::prepare_clip(&source).expect("prepare cached source");
    let cached_ms = cached_start.elapsed().as_millis();
    assert_eq!(first.visuals.poster, second.visuals.poster);
    assert_eq!(first.visuals.filmstrip, second.visuals.filmstrip);
    for (path, original) in paths.iter().zip(stamps) {
        assert_eq!(std::fs::metadata(path).unwrap().modified().unwrap(), original, "cache reused without rewriting");
    }
    println!("{}", serde_json::json!({
        "verdict":"PASS", "source":first.info.path, "duration":first.info.duration,
        "width":first.info.width,"height":first.info.height,
        "first_prepare_ms":first_ms,"cached_prepare_ms":cached_ms,
        "poster":first.visuals.poster,"filmstrip":first.visuals.filmstrip,"aspect":first.visuals.aspect,
        "warning":first.warning,
        "tracks":first.info.audio.iter().map(|track|serde_json::json!({
            "name":track.name,"stream_index":track.stream_index,"channels":track.channels,
            "sample_rate":track.sample_rate,"waveform_bins":track.waveform.len(),
            "peak":track.waveform.iter().copied().fold(0.0_f32,f32::max)
        })).collect::<Vec<_>>()
    }));
}
