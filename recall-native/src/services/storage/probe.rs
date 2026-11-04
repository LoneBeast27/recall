//! What a clip is, read from its index alone (no decoding), by ffprobe. Same numbers as the
//! engine's `prepare-editor`, so a clip can open before its media is prepared.
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use super::ffmpeg;

#[derive(Debug, Clone, PartialEq)]
pub struct ProbedTrack {
    pub stream_index: usize,
    pub name: Option<String>,
    pub channels: u16,
    /// The recorder's source for this track (`edit_model::AUDIO_SOURCES_TAG`), "" when untagged.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProbedClip {
    /// Length on the editor timeline, and where that timeline starts in the file.
    pub duration: f64,
    pub origin: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub tracks: Vec<ProbedTrack>,
}

pub fn probe_clip(source: &Path, lowered: bool) -> Result<ProbedClip, String> {
    let mut command = Command::new(ffmpeg::binary("RECALL_FFPROBE", "ffprobe.exe")?);
    command
        .args(["-v", "error", "-of", "json", "-show_entries"])
        .arg(format!("format=duration:format_tags={}:stream=index,codec_type,width,height,avg_frame_rate,start_time,duration,channels:stream_tags=title,handler_name,language", crate::edit_model::AUDIO_SOURCES_TAG))
        .arg(source);
    let output = ffmpeg::run_hidden(&mut command, lowered).map_err(|error| format!("start ffprobe: {error}"))?;
    if !output.status.success() {
        return Err(format!("ffprobe: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let report: Value = serde_json::from_slice(&output.stdout).map_err(|error| format!("read ffprobe: {error}"))?;
    parse(&report)
}

fn seconds(value: &Value, key: &str) -> Option<f64> {
    value[key].as_str()?.parse().ok().filter(|v: &f64| v.is_finite())
}

/// Stream title, else a handler name that is not ffmpeg's placeholder, else the language
/// (`recall/src/editor_media.rs::track_name`).
fn track_name(tags: &Value) -> Option<String> {
    let usable = |key: &str| tags[key].as_str().map(str::trim).filter(|v| !v.is_empty());
    usable("title").or_else(|| usable("handler_name").filter(|v| *v != "SoundHandler")).or_else(|| usable("language")).map(str::to_owned)
}

fn parse(report: &Value) -> Result<ProbedClip, String> {
    let streams = report["streams"].as_array().ok_or("ffprobe listed no streams")?;
    let kind = |stream: &Value| stream["codec_type"].as_str().map(str::to_owned);
    let video = streams.iter().find(|stream| kind(stream).as_deref() == Some("video")).ok_or("clip has no video stream")?;
    let timed: Vec<&Value> = streams.iter().filter(|stream| std::ptr::eq(*stream, video) || kind(stream).as_deref() == Some("audio")).collect();
    let origin = timed.iter().filter_map(|stream| seconds(stream, "start_time")).reduce(f64::min).unwrap_or(0.0);
    let mut duration = seconds(&report["format"], "duration").unwrap_or(0.0).max(0.0);
    for stream in &timed {
        if let Some(length) = seconds(stream, "duration").filter(|length| *length > 0.0) {
            duration = duration.max(seconds(stream, "start_time").unwrap_or(origin) + length - origin);
        }
    }
    if duration <= 0.0 {
        return Err("clip has no length".into());
    }
    let fps = video["avg_frame_rate"].as_str().and_then(|rate| {
        let (top, bottom) = rate.split_once('/')?;
        let bottom: f64 = bottom.parse().ok()?;
        (bottom != 0.0).then(|| top.parse::<f64>().ok().map(|top| top / bottom))?
    });
    let audio_count = streams.iter().filter(|stream| kind(stream).as_deref() == Some("audio")).count();
    // A tag for another number of tracks (the file was edited elsewhere) counts as none.
    let sources = crate::edit_model::audio_sources_for(report["format"]["tags"][crate::edit_model::AUDIO_SOURCES_TAG].as_str(), audio_count);
    let tracks = streams
        .iter()
        .filter(|stream| kind(stream).as_deref() == Some("audio"))
        .enumerate()
        .map(|(ordinal, stream)| ProbedTrack {
            stream_index: stream["index"].as_u64().unwrap_or(0) as usize,
            name: track_name(&stream["tags"]),
            channels: stream["channels"].as_u64().unwrap_or(2).clamp(1, 2) as u16,
            source: sources.get(ordinal).cloned().unwrap_or_default(),
        })
        .collect();
    Ok(ProbedClip {
        duration,
        origin,
        width: video["width"].as_u64().unwrap_or(0) as u32,
        height: video["height"].as_u64().unwrap_or(0) as u32,
        fps: fps.unwrap_or(0.0),
        tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_stream_sets_the_length_from_the_earliest_start() {
        let report = serde_json::json!({
            "format": {"duration": "10.000000"},
            "streams": [
                {"index": 0, "codec_type": "video", "width": 1920, "height": 1080, "avg_frame_rate": "60/1", "start_time": "0.100000", "duration": "10.000000"},
                {"index": 1, "codec_type": "audio", "channels": 2, "start_time": "0.000000", "duration": "10.500000", "tags": {"handler_name": "SoundHandler", "language": "und"}},
                {"index": 2, "codec_type": "audio", "channels": 6, "start_time": "0.050000", "tags": {"handler_name": "Mic"}},
                {"index": 3, "codec_type": "data"},
            ],
        });
        let clip = parse(&report).unwrap();
        assert!((clip.duration - 10.5).abs() < 1e-9);
        assert_eq!((clip.width, clip.height, clip.fps), (1920, 1080, 60.0));
        assert_eq!(clip.tracks, vec![
            ProbedTrack { stream_index: 1, name: Some("und".into()), channels: 2, source: String::new() },
            ProbedTrack { stream_index: 2, name: Some("Mic".into()), channels: 2, source: String::new() },
        ]);
    }

    #[test]
    fn each_track_gets_its_source_from_the_file_tag() {
        let report = serde_json::json!({
            "format": {"duration": "2.0", "tags": {"recall_audio_sources": "[\"desktop-loopback\",\"mic\",\"app:a.exe\"]"}},
            "streams": [
                {"index": 0, "codec_type": "video", "width": 4, "height": 2, "avg_frame_rate": "60/1"},
                {"index": 1, "codec_type": "audio", "channels": 2},
                {"index": 2, "codec_type": "audio", "channels": 2},
                {"index": 3, "codec_type": "audio", "channels": 2},
            ],
        });
        let sources: Vec<String> = parse(&report).unwrap().tracks.into_iter().map(|t| t.source).collect();
        assert_eq!(sources, ["desktop-loopback", "mic", "app:a.exe"]);
        let untagged = serde_json::json!({"format": {"duration": "2.0"}, "streams": [{"index": 0, "codec_type": "video"}, {"index": 1, "codec_type": "audio"}]});
        assert_eq!(parse(&untagged).unwrap().tracks[0].source, "", "older clips: no source");
        let mut short = report.clone();
        short["format"]["tags"]["recall_audio_sources"] = serde_json::json!("[\"desktop-loopback\",\"app:a.exe\"]");
        assert!(parse(&short).unwrap().tracks.iter().all(|t| t.source.is_empty()), "2 sources for 3 tracks: untagged");
    }

    #[test]
    fn a_clip_without_video_or_length_cannot_stream() {
        assert!(parse(&serde_json::json!({"format": {}, "streams": [{"codec_type": "audio"}]})).is_err());
        assert!(parse(&serde_json::json!({"format": {}, "streams": [{"codec_type": "video"}]})).is_err());
    }
}
