//! Programmatic VFR-to-CFR export using NVIDIA's H.264 encoder.
//!
//! Source video is decoded once. Output ticks use a `1/fps` timebase and each
//! tick receives the latest decoded frame whose source timestamp is not later
//! than that tick. This deliberately repeats or drops captured frames; it does
//! not synthesize motion. Every audio stream is remuxed with rebased timestamps.

#![allow(unexpected_cfgs)]

#[cfg(not(sampler_only))]
use std::path::{Path, PathBuf};

#[cfg(not(sampler_only))]
use ff::Rescale;
#[cfg(not(sampler_only))]
use ffmpeg_next as ff;

#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub input: String,
    pub output: String,
    pub fps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportResult {
    pub frames: u64,
    pub seconds: f64,
}

/// Number of CFR ticks strictly before `time_us`.
///
/// This is `ceil(time * fps)`, expressed with integers so high-rate exports do
/// not accumulate floating-point drift.
fn ticks_before(time_us: i64, fps: u32) -> u64 {
    if time_us <= 0 || fps == 0 {
        return 0;
    }
    let scaled = time_us as i128 * fps as i128;
    ((scaled + 999_999) / 1_000_000) as u64
}

#[cfg(not(sampler_only))]
pub(crate) fn receive_is_drained(error: &ff::Error) -> bool {
    match error {
        ff::Error::Eof => true,
        ff::Error::Other { errno } => *errno == ff::error::EAGAIN,
        _ => false,
    }
}

#[cfg(not(sampler_only))]
pub fn export_cfr(req: &ExportRequest) -> Result<ExportResult, String> {
    validate_request(req)?;
    ff::init().map_err(|e| format!("FFmpeg initialization failed: {e}"))?;

    let input = canonical_input(Path::new(&req.input))?;
    let output = absolute_output(Path::new(&req.output))?;
    if input == output {
        return Err("input and output must be different files".into());
    }
    if output.exists() {
        return Err(format!("destination already exists: {}", output.display()));
    }
    let temp = temporary_output_path(&output)?;

    let result = export_to_temp(&input, &temp, req.fps);
    match result {
        Ok(exported) => {
            if output.exists() {
                let _ = std::fs::remove_file(&temp);
                return Err(format!(
                    "destination appeared during export: {}",
                    output.display()
                ));
            }
            std::fs::rename(&temp, &output).map_err(|e| {
                let _ = std::fs::remove_file(&temp);
                format!("publish {}: {e}", output.display())
            })?;
            Ok(exported)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

/// Export a nondestructive edit plan as gapless CFR video plus separate AAC
/// audio streams. The source and final destination use the same protection and
/// atomic-publish rules as [`export_cfr`].
#[cfg(not(sampler_only))]
pub fn export_edited(
    req: &ExportRequest,
    plan: &crate::edit_model::EditPlan,
) -> Result<ExportResult, String> {
    validate_request(req)?;
    ff::init().map_err(|e| format!("FFmpeg initialization failed: {e}"))?;

    let input = canonical_input(Path::new(&req.input))?;
    let output = absolute_output(Path::new(&req.output))?;
    if input == output {
        return Err("input and output must be different files".into());
    }
    if output.exists() {
        return Err(format!("destination already exists: {}", output.display()));
    }
    let temp = temporary_output_path(&output)?;

    let result = export_edited_to_temp(&input, &temp, req.fps, plan);
    match result {
        Ok(exported) => {
            if output.exists() {
                let _ = std::fs::remove_file(&temp);
                return Err(format!(
                    "destination appeared during export: {}",
                    output.display()
                ));
            }
            std::fs::rename(&temp, &output).map_err(|e| {
                let _ = std::fs::remove_file(&temp);
                format!("publish {}: {e}", output.display())
            })?;
            Ok(exported)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

#[cfg(not(sampler_only))]
fn validate_request(req: &ExportRequest) -> Result<(), String> {
    if req.fps == 0 {
        return Err("export fps must be a positive integer".into());
    }
    if req.fps > i32::MAX as u32 {
        return Err(format!(
            "export fps {} exceeds FFmpeg's supported integer range",
            req.fps
        ));
    }
    if req.input.trim().is_empty() {
        return Err("input path is empty".into());
    }
    if req.output.trim().is_empty() {
        return Err("output path is empty".into());
    }
    Ok(())
}

#[cfg(not(sampler_only))]
fn canonical_input(path: &Path) -> Result<PathBuf, String> {
    if !path.is_file() {
        return Err(format!("input video does not exist: {}", path.display()));
    }
    std::fs::canonicalize(path).map_err(|e| format!("resolve input {}: {e}", path.display()))
}

#[cfg(not(sampler_only))]
fn absolute_output(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| "output path has no file name".to_string())?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = std::fs::canonicalize(parent)
        .map_err(|e| format!("resolve output directory {}: {e}", parent.display()))?;
    if !parent.is_dir() {
        return Err(format!(
            "output directory is not a directory: {}",
            parent.display()
        ));
    }
    Ok(parent.join(name))
}

#[cfg(not(sampler_only))]
fn temporary_output_path(output: &Path) -> Result<PathBuf, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let parent = output
        .parent()
        .ok_or_else(|| "output path has no parent directory".to_string())?;
    let stem = output
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("export");
    let extension = output.extension().and_then(|s| s.to_str()).unwrap_or("mp4");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock before Unix epoch: {e}"))?
        .as_nanos();
    for attempt in 0..32u32 {
        let candidate = parent.join(format!(
            ".{stem}.recall-export-{}-{nonce}-{attempt}.{extension}",
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                drop(file);
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "reserve temporary output {}: {error}",
                    candidate.display()
                ))
            }
        }
    }
    Err("could not allocate a unique temporary export path".into())
}

#[cfg(not(sampler_only))]
fn export_to_temp(input: &Path, temp: &Path, fps: u32) -> Result<ExportResult, String> {
    let mut ictx =
        ff::format::input(input).map_err(|e| format!("open input {}: {e}", input.display()))?;

    let video_index = ictx
        .streams()
        .best(ff::media::Type::Video)
        .map(|s| s.index())
        .ok_or_else(|| "input has no decodable video stream".to_string())?;

    let (video_tb, video_params, width, height, source_format, aspect_ratio) = {
        let stream = ictx
            .stream(video_index)
            .ok_or_else(|| "video stream disappeared".to_string())?;
        let decoder = ff::codec::context::Context::from_parameters(stream.parameters())
            .map_err(|e| format!("create video decoder: {e}"))?
            .decoder()
            .video()
            .map_err(|e| format!("open video decoder: {e}"))?;
        (
            stream.time_base(),
            stream.parameters(),
            decoder.width(),
            decoder.height(),
            decoder.format(),
            decoder.aspect_ratio(),
        )
    };
    if width == 0 || height == 0 {
        return Err("input video has invalid zero dimensions".into());
    }

    let mut decoder = ff::codec::context::Context::from_parameters(video_params)
        .map_err(|e| format!("create video decoder: {e}"))?
        .decoder()
        .video()
        .map_err(|e| format!("open video decoder: {e}"))?;
    let mut scaler = ff::software::scaling::Context::get(
        source_format,
        width,
        height,
        ff::format::Pixel::YUV420P,
        width,
        height,
        ff::software::scaling::Flags::BILINEAR,
    )
    .map_err(|e| format!("create video pixel converter: {e}"))?;

    let nvenc = ff::encoder::find_by_name("h264_nvenc").ok_or_else(|| {
        "NVIDIA NVENC encoder h264_nvenc is unavailable; no CPU fallback is permitted".to_string()
    })?;
    let output_tb = ff::Rational(1, fps as i32);
    let mut enc_ctx = ff::codec::context::Context::new_with_codec(nvenc)
        .encoder()
        .video()
        .map_err(|e| format!("create h264_nvenc context: {e}"))?;
    enc_ctx.set_width(width);
    enc_ctx.set_height(height);
    enc_ctx.set_aspect_ratio(aspect_ratio);
    enc_ctx.set_format(ff::format::Pixel::YUV420P);
    enc_ctx.set_time_base(output_tb);
    enc_ctx.set_frame_rate(Some(ff::Rational(fps as i32, 1)));
    enc_ctx.set_gop(fps.max(1));
    enc_ctx.set_max_b_frames(0);
    enc_ctx.set_bit_rate(recommended_bitrate(width, height, fps));
    enc_ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut options = ff::Dictionary::new();
    options.set("preset", "p5");
    options.set("tune", "hq");
    options.set("rc", "vbr");
    options.set("bf", "0");
    let encoder = enc_ctx.open_with(options).map_err(|e| {
        format!("open NVIDIA NVENC encoder h264_nvenc: {e}; no CPU fallback is permitted")
    })?;

    let mut octx = ff::format::output(temp)
        .map_err(|e| format!("create temporary output {}: {e}", temp.display()))?;
    let video_out_index = {
        let mut stream = octx
            .add_stream(Some(nvenc))
            .map_err(|e| format!("add output video stream: {e}"))?;
        stream.set_parameters(&encoder);
        stream.set_time_base(output_tb);
        stream.index()
    };

    let mut stream_map = vec![None; ictx.nb_streams() as usize];
    let mut input_time_bases = vec![ff::Rational(0, 1); ictx.nb_streams() as usize];
    for stream in ictx.streams() {
        input_time_bases[stream.index()] = stream.time_base();
        if stream.parameters().medium() != ff::media::Type::Audio {
            continue;
        }
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::None))
            .map_err(|e| format!("add output audio stream {}: {e}", stream.index()))?;
        out.set_parameters(stream.parameters());
        unsafe {
            (*out.parameters().as_mut_ptr()).codec_tag = 0;
        }
        stream_map[stream.index()] = Some(out.index());
    }
    octx.set_metadata(ictx.metadata().to_owned());

    let origin_us = timeline_origin_us(&ictx, video_index);
    let planned_duration_us = timeline_duration_us(&ictx, origin_us);
    octx.write_header()
        .map_err(|e| format!("write output header: {e}"))?;
    let output_time_bases: Vec<_> = octx.streams().map(|s| s.time_base()).collect();

    let mut pipeline = VideoPipeline {
        decoder: &mut decoder,
        scaler: &mut scaler,
        encoder,
        source_time_base: video_tb,
        output_time_base: output_tb,
        output_stream: video_out_index,
        origin_us,
        held: None,
        cursor: 0,
        newest_frame_us: 0,
    };
    let mut observed_end_us = 0i64;

    for (stream, mut packet) in ictx.packets() {
        let stream_index = stream.index();
        let source_tb = input_time_bases[stream_index];
        if let Some(timestamp) = packet.pts().or(packet.dts()) {
            let end = timestamp.saturating_add(packet.duration().max(0));
            let end_us = end
                .rescale(source_tb, ff::rescale::TIME_BASE)
                .saturating_sub(origin_us);
            observed_end_us = observed_end_us.max(end_us);
        }

        if stream_index == video_index {
            pipeline.send_packet(&packet, &mut octx)?;
        } else if let Some(output_index) = stream_map[stream_index] {
            let origin_in_stream = origin_us.rescale(ff::rescale::TIME_BASE, source_tb);
            packet.set_pts(packet.pts().map(|v| v.saturating_sub(origin_in_stream)));
            packet.set_dts(packet.dts().map(|v| v.saturating_sub(origin_in_stream)));
            packet.rescale_ts(source_tb, output_time_bases[output_index]);
            packet.set_position(-1);
            packet.set_stream(output_index);
            packet
                .write_interleaved(&mut octx)
                .map_err(|e| format!("write audio stream {stream_index}: {e}"))?;
        }
    }

    pipeline.finish_decoder(&mut octx)?;
    let duration_us = planned_duration_us
        .max(observed_end_us)
        .max(pipeline.newest_frame_us);
    pipeline.emit_until(ticks_before(duration_us, fps), &mut octx)?;
    if pipeline.cursor == 0 {
        return Err("input video decoded no frames".into());
    }
    pipeline.finish_encoder(&mut octx)?;
    let frames = pipeline.cursor;
    drop(pipeline);
    octx.write_trailer()
        .map_err(|e| format!("write output trailer: {e}"))?;

    Ok(ExportResult {
        frames,
        seconds: frames as f64 / fps as f64,
    })
}

#[cfg(not(sampler_only))]
fn export_edited_to_temp(
    input: &Path,
    temp: &Path,
    fps: u32,
    plan: &crate::edit_model::EditPlan,
) -> Result<ExportResult, String> {
    let mut ictx =
        ff::format::input(input).map_err(|e| format!("open input {}: {e}", input.display()))?;
    let video_index = ictx
        .streams()
        .best(ff::media::Type::Video)
        .map(|stream| stream.index())
        .ok_or_else(|| "input has no decodable video stream".to_string())?;
    let (video_tb, video_params, width, height, source_format, aspect_ratio) = {
        let stream = ictx
            .stream(video_index)
            .ok_or_else(|| "video stream disappeared".to_string())?;
        let decoder = ff::codec::context::Context::from_parameters(stream.parameters())
            .map_err(|e| format!("create video decoder: {e}"))?
            .decoder()
            .video()
            .map_err(|e| format!("open video decoder: {e}"))?;
        (
            stream.time_base(),
            stream.parameters(),
            decoder.width(),
            decoder.height(),
            decoder.format(),
            decoder.aspect_ratio(),
        )
    };
    if width == 0 || height == 0 {
        return Err("input video has invalid zero dimensions".into());
    }
    let origin_us = timeline_origin_us(&ictx, video_index);
    let source_duration_us = timeline_duration_us(&ictx, origin_us);
    let audio_indices: Vec<_> = ictx
        .streams()
        .filter(|stream| stream.parameters().medium() == ff::media::Type::Audio)
        .map(|stream| stream.index())
        .collect();
    plan.validate(source_duration_us as f64 / 1_000_000.0, &audio_indices)?;
    let timeline = EditTimeline::from_plan(plan)?;
    let mut audio = crate::edit_audio::encode_edited_audio(input, origin_us, plan)?;

    let mut decoder = ff::codec::context::Context::from_parameters(video_params)
        .map_err(|e| format!("create video decoder: {e}"))?
        .decoder()
        .video()
        .map_err(|e| format!("open video decoder: {e}"))?;
    let mut scaler = ff::software::scaling::Context::get(
        source_format,
        width,
        height,
        ff::format::Pixel::YUV420P,
        width,
        height,
        ff::software::scaling::Flags::BILINEAR,
    )
    .map_err(|e| format!("create video pixel converter: {e}"))?;

    let nvenc = ff::encoder::find_by_name("h264_nvenc").ok_or_else(|| {
        "NVIDIA NVENC encoder h264_nvenc is unavailable; no CPU fallback is permitted".to_string()
    })?;
    let output_tb = ff::Rational(1, fps as i32);
    let mut enc_ctx = ff::codec::context::Context::new_with_codec(nvenc)
        .encoder()
        .video()
        .map_err(|e| format!("create h264_nvenc context: {e}"))?;
    enc_ctx.set_width(width);
    enc_ctx.set_height(height);
    enc_ctx.set_aspect_ratio(aspect_ratio);
    enc_ctx.set_format(ff::format::Pixel::YUV420P);
    enc_ctx.set_time_base(output_tb);
    enc_ctx.set_frame_rate(Some(ff::Rational(fps as i32, 1)));
    enc_ctx.set_gop(fps.max(1));
    enc_ctx.set_max_b_frames(0);
    enc_ctx.set_bit_rate(recommended_bitrate(width, height, fps));
    enc_ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    let mut options = ff::Dictionary::new();
    options.set("preset", "p5");
    options.set("tune", "hq");
    options.set("rc", "vbr");
    options.set("bf", "0");
    let encoder = enc_ctx.open_with(options).map_err(|e| {
        format!("open NVIDIA NVENC encoder h264_nvenc: {e}; no CPU fallback is permitted")
    })?;

    let mut octx = ff::format::output(temp)
        .map_err(|e| format!("create temporary output {}: {e}", temp.display()))?;
    let video_out_index = {
        let mut stream = octx
            .add_stream(Some(nvenc))
            .map_err(|e| format!("add output video stream: {e}"))?;
        stream.set_parameters(&encoder);
        stream.set_time_base(output_tb);
        stream.index()
    };
    let mut audio_output_indices = Vec::with_capacity(audio.len());
    for track in &audio {
        let mut stream = octx
            .add_stream(ff::encoder::find(ff::codec::Id::AAC))
            .map_err(|e| format!("add output audio stream {}: {e}", track.source_index))?;
        stream.set_parameters(&track.encoder);
        stream.set_time_base(ff::Rational(1, track.rate as i32));
        audio_output_indices.push(stream.index());
    }
    octx.set_metadata(ictx.metadata().to_owned());
    octx.write_header()
        .map_err(|e| format!("write output header: {e}"))?;
    let mut audio_mux = EditedAudioMux::new(&mut audio, audio_output_indices);
    let mut pipeline = EditedVideoPipeline {
        decoder: &mut decoder,
        scaler: &mut scaler,
        encoder,
        source_time_base: video_tb,
        output_time_base: output_tb,
        output_stream: video_out_index,
        origin_us,
        timeline,
        range_index: 0,
        held: None,
        cursor: 0,
        decoded_frames: 0,
    };

    for (stream, packet) in ictx.packets() {
        if stream.index() == video_index {
            pipeline.send_packet(&packet, &mut octx, &mut audio_mux)?;
        }
    }
    pipeline.finish_decoder(&mut octx, &mut audio_mux)?;
    pipeline.finish_ranges(&mut octx, &mut audio_mux)?;
    if pipeline.decoded_frames == 0 || pipeline.cursor == 0 {
        return Err("input video decoded no frames for the edit plan".into());
    }
    pipeline.finish_encoder(&mut octx, &mut audio_mux)?;
    let frames = pipeline.cursor;
    drop(pipeline);
    audio_mux.finish(&mut octx)?;
    octx.write_trailer()
        .map_err(|e| format!("write output trailer: {e}"))?;

    Ok(ExportResult {
        frames,
        seconds: frames as f64 / fps as f64,
    })
}

#[cfg(not(sampler_only))]
#[derive(Debug, Clone, Copy)]
struct EditRangeUs {
    source_start: i64,
    source_end: i64,
    output_start: i64,
    output_end: i64,
}

#[cfg(not(sampler_only))]
struct EditTimeline {
    ranges: Vec<EditRangeUs>,
}

#[cfg(not(sampler_only))]
impl EditTimeline {
    fn from_plan(plan: &crate::edit_model::EditPlan) -> Result<Self, String> {
        let mut output_start = 0i64;
        let mut ranges = Vec::with_capacity(plan.ranges.len());
        for range in &plan.ranges {
            let source_start = seconds_to_us(range.start)?;
            let source_end = seconds_to_us(range.end)?;
            let output_end = output_start
                .checked_add(source_end.saturating_sub(source_start))
                .ok_or_else(|| "edited video duration is too large".to_string())?;
            ranges.push(EditRangeUs {
                source_start,
                source_end,
                output_start,
                output_end,
            });
            output_start = output_end;
        }
        Ok(Self { ranges })
    }
}

#[cfg(not(sampler_only))]
fn seconds_to_us(seconds: f64) -> Result<i64, String> {
    let value = seconds * 1_000_000.0;
    if !value.is_finite() || value < 0.0 || value > i64::MAX as f64 {
        return Err("edited video time is outside the supported range".into());
    }
    Ok(value.round() as i64)
}

#[cfg(not(sampler_only))]
struct EditedAudioMux<'a> {
    tracks: &'a mut [crate::edit_audio::EncodedAudioTrack],
    output_indices: Vec<usize>,
    cursors: Vec<usize>,
}

#[cfg(not(sampler_only))]
impl<'a> EditedAudioMux<'a> {
    fn new(
        tracks: &'a mut [crate::edit_audio::EncodedAudioTrack],
        output_indices: Vec<usize>,
    ) -> Self {
        let cursors = vec![0; tracks.len()];
        Self {
            tracks,
            output_indices,
            cursors,
        }
    }

    fn write_before(
        &mut self,
        output: &mut ff::format::context::Output,
        limit_seconds: f64,
    ) -> Result<(), String> {
        loop {
            let mut selected = None;
            let mut selected_seconds = f64::INFINITY;
            for (track_index, track) in self.tracks.iter().enumerate() {
                let Some(packet) = track.packets.get(self.cursors[track_index]) else {
                    continue;
                };
                let timestamp = packet.dts().or(packet.pts()).unwrap_or(0);
                let seconds = timestamp as f64 / track.rate as f64;
                if seconds <= limit_seconds && seconds < selected_seconds {
                    selected = Some(track_index);
                    selected_seconds = seconds;
                }
            }
            let Some(track_index) = selected else {
                break;
            };
            self.write_one(output, track_index)?;
        }
        Ok(())
    }

    fn finish(&mut self, output: &mut ff::format::context::Output) -> Result<(), String> {
        self.write_before(output, f64::INFINITY)
    }

    fn write_one(
        &mut self,
        output: &mut ff::format::context::Output,
        track_index: usize,
    ) -> Result<(), String> {
        let track = &mut self.tracks[track_index];
        let packet = &mut track.packets[self.cursors[track_index]];
        let output_index = self.output_indices[track_index];
        packet.set_stream(output_index);
        packet.rescale_ts(
            ff::Rational(1, track.rate as i32),
            output
                .stream(output_index)
                .ok_or_else(|| "output audio stream disappeared".to_string())?
                .time_base(),
        );
        packet.set_position(-1);
        packet
            .write_interleaved(output)
            .map_err(|e| format!("write edited audio stream {}: {e}", track.source_index))?;
        self.cursors[track_index] += 1;
        Ok(())
    }
}

#[cfg(not(sampler_only))]
struct EditedVideoPipeline<'a> {
    decoder: &'a mut ff::decoder::Video,
    scaler: &'a mut ff::software::scaling::Context,
    encoder: ff::encoder::Video,
    source_time_base: ff::Rational,
    output_time_base: ff::Rational,
    output_stream: usize,
    origin_us: i64,
    timeline: EditTimeline,
    range_index: usize,
    held: Option<ff::frame::Video>,
    cursor: u64,
    decoded_frames: u64,
}

#[cfg(not(sampler_only))]
impl EditedVideoPipeline<'_> {
    fn send_packet(
        &mut self,
        packet: &ff::Packet,
        output: &mut ff::format::context::Output,
        audio: &mut EditedAudioMux<'_>,
    ) -> Result<(), String> {
        self.decoder
            .send_packet(packet)
            .map_err(|e| format!("decode video packet: {e}"))?;
        self.receive_frames(output, audio)
    }

    fn finish_decoder(
        &mut self,
        output: &mut ff::format::context::Output,
        audio: &mut EditedAudioMux<'_>,
    ) -> Result<(), String> {
        self.decoder
            .send_eof()
            .map_err(|e| format!("flush video decoder: {e}"))?;
        self.receive_frames(output, audio)
    }

    fn receive_frames(
        &mut self,
        output: &mut ff::format::context::Output,
        audio: &mut EditedAudioMux<'_>,
    ) -> Result<(), String> {
        let mut decoded = ff::frame::Video::empty();
        loop {
            match self.decoder.receive_frame(&mut decoded) {
                Ok(()) => {
                    self.decoded_frames += 1;
                    let timestamp = decoded.timestamp().or(decoded.pts()).ok_or_else(|| {
                        "decoded video frame has no timestamp; edited CFR sampling requires timestamps"
                            .to_string()
                    })?;
                    let frame_us = timestamp
                        .rescale(self.source_time_base, ff::rescale::TIME_BASE)
                        .saturating_sub(self.origin_us)
                        .max(0);
                    while self.range_index < self.timeline.ranges.len()
                        && frame_us >= self.timeline.ranges[self.range_index].source_end
                    {
                        let target = ticks_before(
                            self.timeline.ranges[self.range_index].output_end,
                            self.output_time_base.1 as u32,
                        );
                        self.emit_until(target, output, audio)?;
                        self.range_index += 1;
                    }
                    if let Some(range) = self.timeline.ranges.get(self.range_index).copied() {
                        if frame_us >= range.source_start && frame_us < range.source_end {
                            let output_us = range
                                .output_start
                                .saturating_add(frame_us.saturating_sub(range.source_start));
                            self.emit_until(
                                ticks_before(output_us, self.output_time_base.1 as u32),
                                output,
                                audio,
                            )?;
                        }
                    }
                    let mut converted = ff::frame::Video::new(
                        ff::format::Pixel::YUV420P,
                        decoded.width(),
                        decoded.height(),
                    );
                    self.scaler
                        .run(&decoded, &mut converted)
                        .map_err(|e| format!("convert decoded video frame: {e}"))?;
                    self.held = Some(converted);
                }
                Err(error) if receive_is_drained(&error) => break,
                Err(error) => return Err(format!("receive decoded video frame: {error}")),
            }
        }
        Ok(())
    }

    fn finish_ranges(
        &mut self,
        output: &mut ff::format::context::Output,
        audio: &mut EditedAudioMux<'_>,
    ) -> Result<(), String> {
        while self.range_index < self.timeline.ranges.len() {
            let target = ticks_before(
                self.timeline.ranges[self.range_index].output_end,
                self.output_time_base.1 as u32,
            );
            self.emit_until(target, output, audio)?;
            self.range_index += 1;
        }
        Ok(())
    }

    fn emit_until(
        &mut self,
        target: u64,
        output: &mut ff::format::context::Output,
        audio: &mut EditedAudioMux<'_>,
    ) -> Result<(), String> {
        let Some(held) = self.held.clone() else {
            return Ok(());
        };
        while self.cursor < target {
            let mut frame = held.clone();
            frame.set_pts(Some(self.cursor as i64));
            frame.set_kind(ff::picture::Type::None);
            self.encoder
                .send_frame(&frame)
                .map_err(|e| format!("send edited frame {} to h264_nvenc: {e}", self.cursor))?;
            self.cursor += 1;
            self.drain_encoder(output, audio)?;
        }
        Ok(())
    }

    fn finish_encoder(
        &mut self,
        output: &mut ff::format::context::Output,
        audio: &mut EditedAudioMux<'_>,
    ) -> Result<(), String> {
        self.encoder
            .send_eof()
            .map_err(|e| format!("flush h264_nvenc: {e}"))?;
        self.drain_encoder(output, audio)
    }

    fn drain_encoder(
        &mut self,
        output: &mut ff::format::context::Output,
        audio: &mut EditedAudioMux<'_>,
    ) -> Result<(), String> {
        let mut packet = ff::Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    let timestamp = packet.dts().or(packet.pts()).unwrap_or(0);
                    audio.write_before(
                        output,
                        timestamp as f64 * self.output_time_base.0 as f64
                            / self.output_time_base.1 as f64,
                    )?;
                    packet.set_stream(self.output_stream);
                    packet.set_duration(1);
                    packet.rescale_ts(
                        self.output_time_base,
                        output
                            .stream(self.output_stream)
                            .ok_or_else(|| "output video stream disappeared".to_string())?
                            .time_base(),
                    );
                    packet.set_position(-1);
                    packet
                        .write_interleaved(output)
                        .map_err(|e| format!("write encoded edited video packet: {e}"))?;
                }
                Err(error) if receive_is_drained(&error) => break,
                Err(error) => return Err(format!("receive h264_nvenc packet: {error}")),
            }
        }
        Ok(())
    }
}

#[cfg(not(sampler_only))]
fn recommended_bitrate(width: u32, height: u32, fps: u32) -> usize {
    let pixels_per_second = width as u128 * height as u128 * fps as u128;
    let bits = (pixels_per_second / 10).clamp(4_000_000, 120_000_000);
    bits as usize
}

#[cfg(not(sampler_only))]
fn timeline_origin_us(ictx: &ff::format::context::Input, video_index: usize) -> i64 {
    let mut origin = None;
    for stream in ictx.streams() {
        let medium = stream.parameters().medium();
        if stream.index() != video_index && medium != ff::media::Type::Audio {
            continue;
        }
        let start = stream.start_time();
        if start == ff::ffi::AV_NOPTS_VALUE {
            continue;
        }
        let value = start.rescale(stream.time_base(), ff::rescale::TIME_BASE);
        origin = Some(origin.map_or(value, |old: i64| old.min(value)));
    }
    origin.unwrap_or(0)
}

#[cfg(not(sampler_only))]
fn timeline_duration_us(ictx: &ff::format::context::Input, origin_us: i64) -> i64 {
    let mut duration = ictx.duration().max(0);
    for stream in ictx.streams() {
        let medium = stream.parameters().medium();
        if medium != ff::media::Type::Video && medium != ff::media::Type::Audio {
            continue;
        }
        if stream.duration() <= 0 || stream.duration() == ff::ffi::AV_NOPTS_VALUE {
            continue;
        }
        let start = if stream.start_time() == ff::ffi::AV_NOPTS_VALUE {
            origin_us.rescale(ff::rescale::TIME_BASE, stream.time_base())
        } else {
            stream.start_time()
        };
        let end_us = start
            .saturating_add(stream.duration())
            .rescale(stream.time_base(), ff::rescale::TIME_BASE)
            .saturating_sub(origin_us);
        duration = duration.max(end_us);
    }
    duration
}

#[cfg(not(sampler_only))]
struct VideoPipeline<'a> {
    decoder: &'a mut ff::decoder::Video,
    scaler: &'a mut ff::software::scaling::Context,
    encoder: ff::encoder::Video,
    source_time_base: ff::Rational,
    output_time_base: ff::Rational,
    output_stream: usize,
    origin_us: i64,
    held: Option<ff::frame::Video>,
    cursor: u64,
    newest_frame_us: i64,
}

#[cfg(not(sampler_only))]
impl VideoPipeline<'_> {
    fn send_packet(
        &mut self,
        packet: &ff::Packet,
        output: &mut ff::format::context::Output,
    ) -> Result<(), String> {
        self.decoder
            .send_packet(packet)
            .map_err(|e| format!("decode video packet: {e}"))?;
        self.receive_frames(output)
    }

    fn finish_decoder(&mut self, output: &mut ff::format::context::Output) -> Result<(), String> {
        self.decoder
            .send_eof()
            .map_err(|e| format!("flush video decoder: {e}"))?;
        self.receive_frames(output)
    }

    fn receive_frames(&mut self, output: &mut ff::format::context::Output) -> Result<(), String> {
        let mut decoded = ff::frame::Video::empty();
        loop {
            match self.decoder.receive_frame(&mut decoded) {
                Ok(()) => {
                    let timestamp = decoded.timestamp().or(decoded.pts()).ok_or_else(|| {
                        "decoded video frame has no timestamp; CFR sampling requires timestamps"
                            .to_string()
                    })?;
                    let frame_us = timestamp
                        .rescale(self.source_time_base, ff::rescale::TIME_BASE)
                        .saturating_sub(self.origin_us)
                        .max(0);
                    self.emit_until(
                        ticks_before(frame_us, self.output_time_base.1 as u32),
                        output,
                    )?;

                    let mut converted = ff::frame::Video::new(
                        ff::format::Pixel::YUV420P,
                        decoded.width(),
                        decoded.height(),
                    );
                    self.scaler
                        .run(&decoded, &mut converted)
                        .map_err(|e| format!("convert decoded video frame: {e}"))?;
                    self.held = Some(converted);
                    self.newest_frame_us = self.newest_frame_us.max(frame_us);
                }
                Err(error) if receive_is_drained(&error) => break,
                Err(error) => return Err(format!("receive decoded video frame: {error}")),
            }
        }
        Ok(())
    }

    fn emit_until(
        &mut self,
        target: u64,
        output: &mut ff::format::context::Output,
    ) -> Result<(), String> {
        let Some(held) = self.held.clone() else {
            return Ok(());
        };
        while self.cursor < target {
            let mut frame = held.clone();
            frame.set_pts(Some(self.cursor as i64));
            frame.set_kind(ff::picture::Type::None);
            self.encoder
                .send_frame(&frame)
                .map_err(|e| format!("send frame {} to h264_nvenc: {e}", self.cursor))?;
            self.cursor += 1;
            self.drain_encoder(output)?;
        }
        Ok(())
    }

    fn finish_encoder(&mut self, output: &mut ff::format::context::Output) -> Result<(), String> {
        self.encoder
            .send_eof()
            .map_err(|e| format!("flush h264_nvenc: {e}"))?;
        self.drain_encoder(output)
    }

    fn drain_encoder(&mut self, output: &mut ff::format::context::Output) -> Result<(), String> {
        let mut packet = ff::Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    packet.set_stream(self.output_stream);
                    packet.set_duration(1);
                    packet.rescale_ts(
                        self.output_time_base,
                        output.stream(self.output_stream).unwrap().time_base(),
                    );
                    packet.set_position(-1);
                    packet
                        .write_interleaved(output)
                        .map_err(|e| format!("write encoded video packet: {e}"))?;
                }
                Err(error) if receive_is_drained(&error) => break,
                Err(error) => return Err(format!("receive h264_nvenc packet: {error}")),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(sampler_only))]
    use super::ff;
    use super::ticks_before;

    fn sampled_sources(source_pts_us: &[i64], duration_us: i64, fps: u32) -> Vec<usize> {
        let mut selected = Vec::new();
        let mut cursor = 0u64;
        let mut held = None;
        for (source_index, &pts) in source_pts_us.iter().enumerate() {
            let target = ticks_before(pts, fps);
            if let Some(index) = held {
                selected.extend((cursor..target).map(|_| index));
                cursor = target;
            }
            held = Some(source_index);
        }
        if let Some(index) = held {
            selected.extend((cursor..ticks_before(duration_us, fps)).map(|_| index));
        }
        selected
    }

    #[test]
    fn exact_boundaries_exclude_the_transition_tick() {
        assert_eq!(ticks_before(0, 60), 0);
        assert_eq!(ticks_before(1_000_000, 60), 60);
        assert_eq!(ticks_before(2_000_000, 480), 960);
    }

    #[test]
    fn fractional_boundaries_round_up_without_float_drift() {
        assert_eq!(ticks_before(1, 60), 1);
        assert_eq!(ticks_before(16_666, 60), 1);
        assert_eq!(ticks_before(16_667, 60), 2);
        assert_eq!(ticks_before(1_001_001, 960), 961);
    }

    #[test]
    fn nonpositive_time_emits_no_ticks() {
        assert_eq!(ticks_before(-1, 60), 0);
        assert_eq!(ticks_before(i64::MIN, 960), 0);
    }

    #[test]
    fn high_rate_long_duration_does_not_overflow() {
        assert_eq!(ticks_before(86_400_000_000, 960), 82_944_000);
    }

    #[test]
    fn sampler_holds_gaps_and_drops_frames_between_ticks() {
        assert_eq!(
            sampled_sources(&[0, 40_000, 41_000, 100_000], 120_000, 60),
            vec![0, 0, 0, 2, 2, 2, 3, 3]
        );
    }

    #[test]
    fn sampler_backfills_from_first_frame_and_keeps_terminal_gap() {
        assert_eq!(
            sampled_sources(&[20_000, 50_000], 100_000, 60),
            vec![0, 0, 0, 1, 1, 1]
        );
    }

    #[test]
    fn edited_ranges_keep_boundary_frames_and_close_gaps() {
        fn sampled(source_pts_us: &[i64], ranges: &[(i64, i64)], fps: u32) -> Vec<usize> {
            let mut output = Vec::new();
            let mut held = None;
            let mut range_index = 0usize;
            let mut cursor = 0u64;
            let mut output_start = 0i64;
            for (source_index, &pts) in source_pts_us.iter().enumerate() {
                while range_index < ranges.len() && pts >= ranges[range_index].1 {
                    let duration = ranges[range_index].1 - ranges[range_index].0;
                    let target = ticks_before(output_start + duration, fps);
                    if let Some(index) = held {
                        output.extend((cursor..target).map(|_| index));
                        cursor = target;
                    }
                    output_start += duration;
                    range_index += 1;
                }
                if let Some(&(start, end)) = ranges.get(range_index) {
                    if pts >= start && pts < end {
                        let target = ticks_before(output_start + pts - start, fps);
                        if let Some(index) = held {
                            output.extend((cursor..target).map(|_| index));
                            cursor = target;
                        }
                    }
                }
                held = Some(source_index);
            }
            while range_index < ranges.len() {
                let duration = ranges[range_index].1 - ranges[range_index].0;
                let target = ticks_before(output_start + duration, fps);
                if let Some(index) = held {
                    output.extend((cursor..target).map(|_| index));
                    cursor = target;
                }
                output_start += duration;
                range_index += 1;
            }
            output
        }

        assert_eq!(
            sampled(
                &[0, 500_000, 1_000_000, 1_500_000, 2_000_000, 2_500_000],
                &[(500_000, 1_500_000), (2_000_000, 2_500_000)],
                2,
            ),
            vec![1, 2, 4]
        );
    }

    #[cfg(not(sampler_only))]
    #[test]
    fn receive_classifier_only_accepts_eagain_and_eof() {
        assert!(super::receive_is_drained(&ff::Error::Eof));
        assert!(super::receive_is_drained(&ff::Error::Other {
            errno: ff::error::EAGAIN,
        }));
        assert!(!super::receive_is_drained(&ff::Error::InvalidData));
        assert!(!super::receive_is_drained(&ff::Error::Other {
            errno: ff::error::EIO,
        }));
    }
}
