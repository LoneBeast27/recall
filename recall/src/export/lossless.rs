//! Lossless trim: kept video packets are stream-copied byte for byte.
//!
//! Each kept range starts at the keyframe at or before its start and runs, in
//! decode order, up to the first packet whose decode time reaches its end (a
//! decode-order prefix of a GOP always decodes). Ranges whose snapped starts
//! overlap the previous range are merged. Segments are concatenated with
//! rebased timestamps. Audio tracks with no gain are stream-copied (packets
//! whose midpoint lies inside a segment); a mix or a track with gain is
//! re-encoded to AAC over exactly the snapped segments; muted tracks are dropped.

use std::path::Path;

use ff::Rescale;
use ffmpeg_next as ff;

use super::audio_mux::EditedAudioMux;
use super::paths::file_bytes;
use super::project::{EditedExport, ExportedRange};
use super::timeline::{seconds_to_us, timeline_duration_us, timeline_origin_us, us_to_seconds};
use super::video_encoder::probe_video;
use crate::edit_model::{AudioOutput, EditPlan, ExportMode, TimeRange};

const US: ff::Rational = ff::rescale::TIME_BASE;

/// Timestamps of one source video packet (stream time base, decode order).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VideoPacket {
    pub pts: i64,
    pub dts: i64,
    pub duration: i64,
    pub key: bool,
}

/// A run of source video packets `first..=last` copied as one piece.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Segment {
    pub first: usize,
    pub last: usize,
    /// Source pts of the snapped keyframe and end of the last frame.
    pub start: i64,
    pub end: i64,
    /// Added to every timestamp of the segment (video time base).
    pub shift: i64,
    pub plan_ranges: Vec<usize>,
}

struct Source {
    video_index: usize,
    video_tb: ff::Rational,
    origin_us: i64,
    width: u32,
    height: u32,
    packets: Vec<VideoPacket>,
    audio: Vec<(usize, ff::Rational)>,
    duration_us: i64,
}

pub(super) fn export(input: &Path, temp: &Path, plan: &EditPlan) -> Result<EditedExport, String> {
    let source = scan(input)?;
    let audio_indices: Vec<_> = source.audio.iter().map(|(index, _)| *index).collect();
    plan.validate(us_to_seconds(source.duration_us), &audio_indices)?;
    let to_ts = |us: i64| (us + source.origin_us).rescale(US, source.video_tb);
    let requested = plan
        .ranges
        .iter()
        .map(|r| Ok((to_ts(seconds_to_us(r.start)?), to_ts(seconds_to_us(r.end)?))))
        .collect::<Result<Vec<_>, String>>()?;
    let segments = plan_segments(&source.packets, &requested)?;
    let to_rel_s = |ts: i64| us_to_seconds(ts.rescale(source.video_tb, US) - source.origin_us);
    let snapped = EditPlan {
        mix_down: plan.mix_down,
        ranges: segments
            .iter()
            .map(|s| TimeRange::new(to_rel_s(s.start), to_rel_s(s.end)))
            .collect(),
        audio: plan.audio.clone(),
    };
    // A mix, or a track with gain, is re-encoded; an untouched track is copied. Muted: dropped.
    let outputs = plan.audio_outputs(&audio_indices);
    let reencode: Vec<AudioOutput> = outputs
        .iter()
        .filter(|output| match output {
            AudioOutput::Mix(_) => true,
            AudioOutput::Track(index) => plan.audio.iter().any(|a| a.stream_index == *index && !a.is_neutral()),
        })
        .cloned()
        .collect();
    let encoded = crate::edit_audio::encode_outputs(input, source.origin_us, &snapped, &reencode)?;
    let frames = write(input, temp, &source, &segments, &outputs, &encoded)?;
    let out_s = |ts: i64| us_to_seconds(ts.rescale(source.video_tb, US));
    let ranges: Vec<_> = segments
        .iter()
        .map(|s| ExportedRange {
            start: to_rel_s(s.start),
            end: to_rel_s(s.end),
            speed: 1.0,
            output_start: out_s(s.start + s.shift),
            output_end: out_s(s.end + s.shift),
            plan_ranges: s.plan_ranges.clone(),
        })
        .collect();
    Ok(EditedExport {
        mode: ExportMode::Lossless,
        frames,
        seconds: ranges.last().map_or(0.0, |r| r.output_end),
        bytes: file_bytes(temp)?,
        width: source.width,
        height: source.height,
        fps: None,
        video_bitrate: None,
        max_bytes: None,
        attempts: 1,
        ranges,
    })
}

/// Demux pass collecting video packet timestamps (no decoding).
fn scan(input: &Path) -> Result<Source, String> {
    let mut ictx =
        ff::format::input(input).map_err(|e| format!("open input {}: {e}", input.display()))?;
    let video = probe_video(&ictx)?;
    let origin_us = timeline_origin_us(&ictx, video.index);
    let duration_us = timeline_duration_us(&ictx, origin_us);
    let audio = ictx
        .streams()
        .filter(|s| s.parameters().medium() == ff::media::Type::Audio)
        .map(|s| (s.index(), s.time_base()))
        .collect();
    let mut packets = Vec::new();
    for (stream, packet) in ictx.packets() {
        if stream.index() != video.index {
            continue;
        }
        let pts = packet.pts().or(packet.dts());
        let dts = packet.dts().or(packet.pts());
        let (Some(pts), Some(dts)) = (pts, dts) else {
            return Err("a video packet has no timestamp; lossless trim needs timestamps".into());
        };
        packets.push(VideoPacket { pts, dts, duration: packet.duration().max(0), key: packet.is_key() });
    }
    fill_missing_durations(&mut packets);
    Ok(Source {
        video_index: video.index,
        video_tb: video.time_base,
        origin_us,
        width: video.width,
        height: video.height,
        packets,
        audio,
        duration_us,
    })
}

/// Packets without a duration last until the next frame in display order; the
/// final one gets the median frame duration.
fn fill_missing_durations(packets: &mut [VideoPacket]) {
    let mut order: Vec<usize> = (0..packets.len()).collect();
    order.sort_by_key(|&i| packets[i].pts);
    let mut gaps: Vec<i64> = order.windows(2).map(|w| packets[w[1]].pts - packets[w[0]].pts).collect();
    for (position, &index) in order.iter().enumerate() {
        if packets[index].duration == 0 {
            if let Some(&next) = order.get(position + 1) {
                packets[index].duration = packets[next].pts - packets[index].pts;
            }
        }
    }
    gaps.sort_unstable();
    let median = gaps.get(gaps.len() / 2).copied().unwrap_or(1).max(1);
    for packet in packets.iter_mut().filter(|p| p.duration == 0) {
        packet.duration = median;
    }
}

/// Snap each requested `[start, end)` (video time base) to a copyable segment,
/// merge overlaps, and lay the segments end to end from time zero.
pub(crate) fn plan_segments(packets: &[VideoPacket], requested: &[(i64, i64)]) -> Result<Vec<Segment>, String> {
    let mut segments: Vec<Segment> = Vec::new();
    for (index, &(start, end)) in requested.iter().enumerate() {
        let keyframe = packets
            .iter()
            .enumerate()
            .filter(|(_, p)| p.key && p.pts <= start)
            .max_by_key(|(_, p)| p.pts)
            .or_else(|| packets.iter().enumerate().filter(|(_, p)| p.key).min_by_key(|(_, p)| p.pts))
            .map(|(i, _)| i)
            .ok_or("the video has no keyframes; lossless trim is impossible")?;
        if packets[keyframe].dts >= end {
            return Err(format!("range {} ends before the first keyframe; widen it or re-encode", index + 1));
        }
        let mut last = keyframe;
        while last + 1 < packets.len() && packets[last + 1].dts < end {
            last += 1;
        }
        match segments.last_mut() {
            Some(previous) if keyframe <= previous.last => {
                previous.last = previous.last.max(last);
                previous.plan_ranges.push(index);
            }
            _ => segments.push(Segment {
                first: keyframe,
                last,
                start: packets[keyframe].pts,
                end: 0,
                shift: 0,
                plan_ranges: vec![index],
            }),
        }
    }
    let mut cursor = 0i64;
    let mut last_dts: Option<i64> = None;
    for segment in &mut segments {
        let run = &packets[segment.first..=segment.last];
        segment.start = run.iter().map(|p| p.pts).min().unwrap_or(segment.start);
        segment.end = run.iter().map(|p| p.pts + p.duration).max().unwrap_or(segment.start);
        let mut shift = cursor - segment.start;
        if let Some(previous) = last_dts {
            shift = shift.max(previous + 1 - run[0].dts);
        }
        segment.shift = shift;
        cursor = segment.end + shift;
        last_dts = run.iter().map(|p| p.dts).max().map(|dts| dts + shift);
    }
    Ok(segments)
}

enum AudioRoute {
    Copy { output: usize, tb: ff::Rational, last_dts: Option<i64> },
    Reencoded,
}

fn write(
    input: &Path,
    temp: &Path,
    source: &Source,
    segments: &[Segment],
    outputs: &[AudioOutput],
    encoded: &[crate::edit_audio::EncodedAudioTrack],
) -> Result<u64, String> {
    let mut ictx =
        ff::format::input(input).map_err(|e| format!("open input {}: {e}", input.display()))?;
    let mut octx = ff::format::output(temp)
        .map_err(|e| format!("create temporary output {}: {e}", temp.display()))?;
    let copy_stream = |octx: &mut ff::format::context::Output, index: usize| -> Result<usize, String> {
        let stream = ictx.stream(index).ok_or("source stream disappeared")?;
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::None))
            .map_err(|e| format!("add output stream {index}: {e}"))?;
        out.set_parameters(stream.parameters());
        out.set_time_base(stream.time_base());
        out.set_metadata(stream.metadata().to_owned());
        // The muxer picks the tag; video gets the one Media Foundation plays (`hvc1`, not
        // FFmpeg's default `hev1`), as the recorder writes it.
        let tag = if index == source.video_index { crate::encoder::mp4_video_tag(stream.parameters().id()) } else { None };
        unsafe {
            (*out.parameters().as_mut_ptr()).codec_tag = tag.unwrap_or(0);
        }
        Ok(out.index())
    };
    let video_out = copy_stream(&mut octx, source.video_index)?;
    let mut routes: Vec<Option<AudioRoute>> = (0..ictx.nb_streams()).map(|_| None).collect();
    // Output order follows `outputs` (the mix first); `encoded` holds the re-encoded ones in
    // that order. A source stream missing from `outputs` (muted, or mixed) has no route.
    let mut encoded_outputs = Vec::new();
    let mut pending = encoded.iter();
    for output in outputs {
        let copy_index = match output {
            AudioOutput::Track(index) if !encoded.iter().any(|t| !t.mixed && t.source_index == *index) => Some(*index),
            _ => None,
        };
        if let Some(index) = copy_index {
            let tb = source.audio.iter().find(|(i, _)| *i == index).map(|(_, tb)| *tb).ok_or("source audio stream disappeared")?;
            let copied = copy_stream(&mut octx, index)?;
            routes[index] = Some(AudioRoute::Copy { output: copied, tb, last_dts: None });
            continue;
        }
        let track = pending.next().ok_or("an edited audio track is missing")?;
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::AAC))
            .map_err(|e| format!("add output audio stream {}: {e}", track.source_index))?;
        out.set_parameters(&track.encoder);
        out.set_time_base(ff::Rational(1, track.rate as i32));
        if let Some(tags) = track.metadata(&ictx) {
            out.set_metadata(tags);
        }
        encoded_outputs.push(out.index());
        if !track.mixed {
            routes[track.source_index] = Some(AudioRoute::Reencoded);
        }
    }
    let audio_indices: Vec<usize> = source.audio.iter().map(|(index, _)| *index).collect();
    let sources = super::sources::for_outputs(&ictx, &audio_indices, outputs);
    super::sources::write_header(&mut octx, &ictx, sources)?;
    let out_tbs: Vec<_> = octx.streams().map(|s| s.time_base()).collect();
    let mut mux = EditedAudioMux::new(encoded, encoded_outputs);
    let windows: Vec<_> = segments
        .iter()
        .map(|s| {
            let start = s.start.rescale(source.video_tb, US);
            let end = s.end.rescale(source.video_tb, US);
            let output_start = (s.start + s.shift).rescale(source.video_tb, US);
            (start, end, output_start)
        })
        .collect();

    let (mut sequence, mut current, mut written) = (0usize, 0usize, 0u64);
    for (stream, mut packet) in ictx.packets() {
        let index = stream.index();
        if index == source.video_index {
            let number = sequence;
            sequence += 1;
            while current < segments.len() && number > segments[current].last {
                current += 1;
            }
            let Some(segment) = segments.get(current).filter(|s| number >= s.first) else {
                continue;
            };
            let meta = source.packets[number];
            packet.set_pts(Some(meta.pts + segment.shift));
            packet.set_dts(Some(meta.dts + segment.shift));
            packet.set_duration(meta.duration);
            mux.write_before(&mut octx, us_to_seconds((meta.dts + segment.shift).rescale(source.video_tb, US)))?;
            packet.rescale_ts(source.video_tb, out_tbs[video_out]);
            packet.set_stream(video_out);
            packet.set_position(-1);
            packet
                .write_interleaved(&mut octx)
                .map_err(|e| format!("write copied video packet: {e}"))?;
            written += 1;
        } else if let Some(Some(AudioRoute::Copy { output, tb, last_dts })) = routes.get_mut(index) {
            let Some(pts) = packet.pts().or(packet.dts()) else { continue };
            let pts_us = pts.rescale(*tb, US);
            let mid_us = pts_us + packet.duration().max(0).rescale(*tb, US) / 2;
            let Some(&(start, _, output_start)) =
                windows.iter().find(|(start, end, _)| mid_us >= *start && mid_us < *end)
            else {
                continue;
            };
            let shift = (output_start - start).rescale(US, *tb);
            let new_pts = pts + shift;
            let new_dts = packet.dts().unwrap_or(pts) + shift;
            if new_pts < 0 || last_dts.is_some_and(|last| new_dts <= last) {
                continue;
            }
            *last_dts = Some(new_dts);
            packet.set_pts(Some(new_pts));
            packet.set_dts(Some(new_dts));
            packet.rescale_ts(*tb, out_tbs[*output]);
            packet.set_stream(*output);
            packet.set_position(-1);
            packet
                .write_interleaved(&mut octx)
                .map_err(|e| format!("write copied audio stream {index}: {e}"))?;
        }
    }
    mux.finish(&mut octx)?;
    if written == 0 {
        return Err("the lossless trim copied no video".into());
    }
    octx.write_trailer().map_err(|e| format!("write output trailer: {e}"))?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::{plan_segments, VideoPacket};

    /// 10 frames of 10 ticks, keyframes every 4 frames (0, 40, 80), no B-frames.
    fn gop_packets() -> Vec<VideoPacket> {
        (0..10)
            .map(|i| VideoPacket { pts: i * 10, dts: i * 10, duration: 10, key: i % 4 == 0 })
            .collect()
    }

    #[test]
    fn cuts_snap_back_to_keyframes_and_concatenate_from_zero() {
        let segments = plan_segments(&gop_packets(), &[(15, 35), (65, 90)]).unwrap();
        assert_eq!((segments[0].first, segments[0].last), (0, 3));
        assert_eq!((segments[0].start, segments[0].end, segments[0].shift), (0, 40, 0));
        assert_eq!((segments[1].first, segments[1].last), (4, 8));
        assert_eq!((segments[1].start, segments[1].end), (40, 90));
        assert_eq!(segments[1].start + segments[1].shift, 40);
    }

    #[test]
    fn overlapping_snaps_merge_into_one_segment() {
        let segments = plan_segments(&gop_packets(), &[(0, 50), (55, 70)]).unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!((segments[0].first, segments[0].last), (0, 6));
        assert_eq!(segments[0].plan_ranges, vec![0, 1]);
    }

    #[test]
    fn gap_between_segments_is_closed() {
        let segments = plan_segments(&gop_packets(), &[(0, 20), (80, 100)]).unwrap();
        assert_eq!(segments[1].start, 80);
        assert_eq!(segments[1].start + segments[1].shift, segments[0].end + segments[0].shift);
    }
}
