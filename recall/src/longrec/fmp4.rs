//! The production long-recording sink: a fragmented MP4
//! (`movflags=frag_keyframe+empty_moov+default_base_moof`, plus a 2 s
//! `frag_duration` so a static screen still closes fragments) with the live
//! encoder's video stream-copied and every audio track AAC-encoded as it
//! arrives, all on the `recall-longrec` thread.
//!
//! I/O goes through a custom AVIO context into [`FileWriter`]: FFmpeg's
//! buffer is larger than any fragment and `flush_packets` is on, so every
//! short callback ends on a fragment boundary. The writer (`fmp4_writer.rs`)
//! appends to RAM and hits the disk in >= 4 MiB sequential writes
//! (SMR-friendly) -- or at the first fragment boundary 5 s after the last
//! write -- always ending on a fragment boundary, so a crash leaves a playable
//! file missing at most a few seconds.

#[path = "fmp4_writer.rs"]
mod writer;

use std::ffi::{c_int, c_void, CString};
use std::fs::{File, OpenOptions};
use std::path::Path;

use ffmpeg_next as ff;
use ff::ffi;

use super::machine::{Cause, Sink, SinkFactory};
use super::{tap, TrackInfo};
use writer::FileWriter;

/// FFmpeg's I/O buffer: larger than a fragment at recording bitrates.
const AVIO_BUFFER: usize = 16 << 20;
const MOVFLAGS: &str = "frag_keyframe+empty_moov+default_base_moof";
const FRAG_DURATION_US: &str = "2000000";
const US: ff::Rational = ff::Rational(1, 1_000_000);
/// `AVERROR(EIO)`.
const AVERROR_EIO: c_int = -5;
/// `AVFMT_AVOID_NEG_TS_MAKE_ZERO`: AAC priming shifts every stream equally.
const AVOID_NEG_TS_MAKE_ZERO: c_int = 2;

/// (payload start, end) of the first `kind` box among the boxes in `data[start..end]`.
fn child_box(data: &[u8], start: usize, end: usize, kind: &[u8; 4]) -> Option<(usize, usize)> {
    let mut pos = start;
    while pos + 8 <= end {
        let size32 = u32::from_be_bytes(data.get(pos..pos + 4)?.try_into().ok()?) as usize;
        let (header, size) = match size32 {
            0 => (8, end - pos),
            1 => (16, u64::from_be_bytes(data.get(pos + 8..pos + 16)?.try_into().ok()?) as usize),
            n => (8, n),
        };
        if size < header || pos + size > end {
            return None;
        }
        if data.get(pos + 4..pos + 8)? == kind {
            return Some((pos + header, pos + size));
        }
        pos += size;
    }
    None
}

/// `empty_moov` writes `moov/mvhd` with duration 0 and no `mvex/mehd`, so
/// the library (which reads `mvhd`, then `mehd`) would show no length.
/// Returns (file offset, new bytes) setting the `mvhd` duration to
/// `duration_us` in the movie timescale, located in the header bytes.
pub(crate) fn mvhd_duration_patch(header: &[u8], duration_us: u64) -> Option<(u64, Vec<u8>)> {
    let (moov, moov_end) = child_box(header, 0, header.len(), b"moov")?;
    let (mvhd, mvhd_end) = child_box(header, moov, moov_end, b"mvhd")?;
    let wide = *header.get(mvhd)? == 1;
    // version+flags (4), creation + modification (2 x 4 or 2 x 8), timescale (4), duration.
    let timescale_at = mvhd + 4 + if wide { 16 } else { 8 };
    let timescale = u32::from_be_bytes(header.get(timescale_at..timescale_at + 4)?.try_into().ok()?) as u128;
    let units = (duration_us as u128 * timescale / 1_000_000) as u64;
    let bytes = if wide { units.to_be_bytes().to_vec() } else { (units.min(u32::MAX as u64) as u32).to_be_bytes().to_vec() };
    let at = timescale_at + 4;
    (timescale > 0 && at + bytes.len() <= mvhd_end).then_some((at as u64, bytes))
}

unsafe extern "C" fn write_packet(opaque: *mut c_void, buf: *const u8, size: c_int) -> c_int {
    if opaque.is_null() || buf.is_null() || size <= 0 {
        return size.max(0);
    }
    // SAFETY: `opaque` is the `FileWriter` boxed by `Fmp4Sink::create`, alive
    // until the sink's Drop (after FFmpeg stops calling back); `buf` holds
    // `size` readable bytes for the duration of this call.
    let writer = unsafe { &mut *(opaque as *mut FileWriter<File>) };
    writer.push(unsafe { std::slice::from_raw_parts(buf, size as usize) });
    if writer.error().is_some() {
        AVERROR_EIO
    } else {
        size
    }
}

/// One audio track's streaming AAC encoder.
struct AacLane {
    encoder: ff::encoder::Audio,
    stream: usize,
    src_channels: usize,
    channels: usize,
    layout: ff::ChannelLayout,
    rate: u32,
    frame_size: usize,
    /// Interleaved samples not yet encoded.
    fifo: Vec<f32>,
    next_pts: i64,
}

impl AacLane {
    fn open(track: &TrackInfo, stream: usize) -> Result<Self, String> {
        let src_channels = track.channels.max(1) as usize;
        let channels = src_channels.min(2);
        let layout = if channels == 1 { ff::ChannelLayout::MONO } else { ff::ChannelLayout::STEREO };
        let codec = ff::encoder::find(ff::codec::Id::AAC).ok_or("aac encoder missing")?;
        let mut ctx = ff::codec::context::Context::new_with_codec(codec).encoder().audio().map_err(|e| e.to_string())?;
        ctx.set_rate(track.rate as i32);
        ctx.set_format(ff::format::Sample::F32(ff::format::sample::Type::Planar));
        ctx.set_channel_layout(layout);
        ctx.set_bit_rate(192_000);
        ctx.set_time_base(ff::Rational(1, track.rate as i32));
        ctx.set_flags(ff::codec::Flags::GLOBAL_HEADER);
        let encoder = ctx.open().map_err(|e| format!("aac open for '{}': {e}", track.name))?;
        let frame_size = if encoder.frame_size() > 0 { encoder.frame_size() as usize } else { 1024 };
        Ok(AacLane { encoder, stream, src_channels, channels, layout, rate: track.rate, frame_size, fifo: Vec::new(), next_pts: 0 })
    }

    fn feed(&mut self, samples: &[f32], octx: &mut ff::format::context::Output, tb: ff::Rational) -> Result<(), String> {
        self.fifo.extend_from_slice(samples);
        let block = self.frame_size * self.src_channels;
        while self.fifo.len() >= block {
            self.encode(self.frame_size, octx, tb)?;
            self.fifo.drain(..block);
        }
        Ok(())
    }

    /// Encode the first `valid` frames of the fifo as one AAC frame (zero-padded).
    fn encode(&mut self, valid: usize, octx: &mut ff::format::context::Output, tb: ff::Rational) -> Result<(), String> {
        let mut frame = ff::frame::Audio::new(ff::format::Sample::F32(ff::format::sample::Type::Planar), self.frame_size, self.layout);
        frame.set_rate(self.rate);
        let src = &self.fifo[..valid * self.src_channels];
        for c in 0..self.channels {
            let plane = frame.plane_mut::<f32>(c);
            for (dst, f) in plane[..valid].iter_mut().zip(src.chunks_exact(self.src_channels)) {
                *dst = f[c];
            }
            plane[valid..].fill(0.0);
        }
        frame.set_pts(Some(self.next_pts));
        self.next_pts += self.frame_size as i64;
        self.encoder.send_frame(&frame).map_err(|e| format!("aac encode: {e}"))?;
        self.receive(octx, tb)
    }

    fn receive(&mut self, octx: &mut ff::format::context::Output, tb: ff::Rational) -> Result<(), String> {
        let mut pkt = ff::Packet::empty();
        while self.encoder.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(self.stream);
            pkt.rescale_ts(ff::Rational(1, self.rate as i32), tb);
            pkt.write_interleaved(octx).map_err(|e| format!("write audio: {e}"))?;
        }
        Ok(())
    }

    fn flush(&mut self, octx: &mut ff::format::context::Output, tb: ff::Rational) -> Result<(), String> {
        let left = self.fifo.len() / self.src_channels;
        if left > 0 {
            self.encode(left, octx, tb)?;
            self.fifo.clear();
        }
        self.encoder.send_eof().map_err(|e| format!("aac flush: {e}"))?;
        self.receive(octx, tb)
    }
}

pub(crate) struct Fmp4Sink {
    octx: Option<ff::format::context::Output>,
    io: *mut ffi::AVIOContext,
    writer: *mut FileWriter<File>,
    audio: Vec<AacLane>,
    out_tbs: Vec<ff::Rational>,
    /// The `ftyp` + empty `moov` as written (for the duration patch).
    header: Vec<u8>,
    /// End of the last video packet, µs from the first.
    video_end_us: u64,
    done: bool,
}

impl Fmp4Sink {
    /// Create `part` (fails if it exists) and write the fragmented-MP4 header.
    pub fn create(part: &Path, video: &ff::codec::Parameters, tracks: &[TrackInfo]) -> Result<Self, String> {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ only: nobody can rename or delete the file while it
        // is being written (the next engine's `.part` recovery included).
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0x1)
            .open(part)
            .map_err(|e| format!("create {}: {e}", part.display()))?;
        let writer = Box::into_raw(Box::new(FileWriter::new(file, AVIO_BUFFER)));
        let mut sink = Fmp4Sink {
            octx: None,
            io: std::ptr::null_mut(),
            writer,
            audio: Vec::new(),
            out_tbs: Vec::new(),
            header: Vec::new(),
            video_end_us: 0,
            done: false,
        };
        sink.open_context(part)?;
        sink.add_streams(video, tracks)?;
        let mut opts = ff::Dictionary::new();
        opts.set("movflags", MOVFLAGS);
        opts.set("frag_duration", FRAG_DURATION_US);
        sink.out().write_header_with(opts).map_err(|e| format!("write fragmented MP4 header: {e}"))?;
        sink.out_tbs = sink.out().streams().map(|s| s.time_base()).collect();
        // SAFETY: `io` is the live custom AVIO context owned by this sink.
        unsafe { ffi::avio_flush(sink.io) };
        sink.header = sink.writer_mut().buffered().to_vec();
        sink.writer_mut().flush_now();
        sink.check_io()?;
        Ok(sink)
    }

    fn open_context(&mut self, part: &Path) -> Result<(), String> {
        // SAFETY: plain FFmpeg allocation calls; every pointer is checked and
        // ownership is recorded in `self` immediately, so Drop frees it.
        unsafe {
            let buffer = ffi::av_malloc(AVIO_BUFFER) as *mut u8;
            if buffer.is_null() {
                return Err("out of memory for the recording buffer".into());
            }
            self.io = ffi::avio_alloc_context(buffer, AVIO_BUFFER as c_int, 1, self.writer as *mut c_void, None, Some(write_packet), None);
            if self.io.is_null() {
                ffi::av_free(buffer as *mut c_void);
                return Err("avio_alloc_context failed".into());
            }
            let format = CString::new("mp4").expect("static name");
            let name = CString::new(part.to_string_lossy().replace('\0', "")).map_err(|e| e.to_string())?;
            let mut ctx: *mut ffi::AVFormatContext = std::ptr::null_mut();
            let r = ffi::avformat_alloc_output_context2(&mut ctx, std::ptr::null(), format.as_ptr(), name.as_ptr());
            if r < 0 || ctx.is_null() {
                return Err(format!("mp4 muxer unavailable ({r})"));
            }
            (*ctx).pb = self.io;
            (*ctx).flags |= ffi::AVFMT_FLAG_CUSTOM_IO;
            (*ctx).flush_packets = 1;
            (*ctx).max_interleave_delta = 2_000_000;
            (*ctx).avoid_negative_ts = AVOID_NEG_TS_MAKE_ZERO;
            self.octx = Some(ff::format::context::Output::wrap(ctx));
        }
        Ok(())
    }

    fn add_streams(&mut self, video: &ff::codec::Parameters, tracks: &[TrackInfo]) -> Result<(), String> {
        let mut ost = self.out().add_stream(None::<ff::Codec>).map_err(|e| format!("video stream: {e}"))?;
        ost.set_parameters(video.clone());
        ost.set_time_base(US);
        for (i, track) in tracks.iter().enumerate() {
            let lane = AacLane::open(track, i + 1)?;
            let mut ast = self.out().add_stream(None::<ff::Codec>).map_err(|e| format!("audio stream: {e}"))?;
            ast.set_parameters(&lane.encoder);
            ast.set_time_base(ff::Rational(1, track.rate as i32));
            self.audio.push(lane);
        }
        Ok(())
    }

    fn out(&mut self) -> &mut ff::format::context::Output {
        self.octx.as_mut().expect("output context is open until drop")
    }

    fn writer_mut(&mut self) -> &mut FileWriter<File> {
        // SAFETY: `writer` is a live Box owned by this sink until Drop.
        unsafe { &mut *self.writer }
    }

    fn check_io(&mut self) -> Result<(), String> {
        match self.writer_mut().error() {
            Some(e) => Err(e.to_string()),
            None => Ok(()),
        }
    }

    fn feed_audio(&mut self, track: usize, samples: &[f32]) -> Result<(), String> {
        let Some(lane) = self.audio.get_mut(track) else { return Ok(()) };
        let tb = self.out_tbs[lane.stream];
        let octx = self.octx.as_mut().expect("output context is open until drop");
        lane.feed(samples, octx, tb)?;
        self.check_io()
    }
}

impl Sink for Fmp4Sink {
    fn video(&mut self, pts_us: u64, duration_us: u64, keyframe: bool, data: &[u8]) -> Result<(), String> {
        let mut pkt = ff::Packet::copy(data);
        pkt.set_pts(Some(pts_us as i64));
        pkt.set_dts(Some(pts_us as i64)); // bf=0: decode order == presentation order
        pkt.set_duration(duration_us as i64);
        if keyframe {
            pkt.set_flags(ff::packet::Flags::KEY);
        }
        pkt.set_stream(0);
        pkt.rescale_ts(US, self.out_tbs[0]);
        pkt.write_interleaved(self.out()).map_err(|e| format!("write video: {e}"))?;
        self.video_end_us = self.video_end_us.max(pts_us + duration_us);
        self.check_io()
    }

    fn audio(&mut self, track: usize, samples: &[f32]) -> Result<(), String> {
        self.feed_audio(track, samples)
    }

    fn silence(&mut self, track: usize, frames: u64) -> Result<(), String> {
        let Some(channels) = self.audio.get(track).map(|l| l.src_channels) else { return Ok(()) };
        let zeros = vec![0.0f32; 4096 * channels];
        let mut left = frames;
        while left > 0 {
            let n = left.min(4096) as usize;
            self.feed_audio(track, &zeros[..n * channels])?;
            left -= n as u64;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<u64, String> {
        for i in 0..self.audio.len() {
            let tb = self.out_tbs[self.audio[i].stream];
            let octx = self.octx.as_mut().expect("output context is open until drop");
            self.audio[i].flush(octx, tb)?;
        }
        // Not `Output::write_trailer`: in fragmented mode movenc returns the
        // `mfra` size (> 0) on success, which ffmpeg-next reports as an error.
        // SAFETY: the context is open and its header was written in `create`.
        let r = unsafe { ffi::av_write_trailer(self.out().as_mut_ptr()) };
        if r < 0 {
            return Err(format!("write trailer: {} ({r})", ff::Error::from(r)));
        }
        // SAFETY: `io` is the live custom AVIO context owned by this sink.
        unsafe { ffi::avio_flush(self.io) };
        self.done = true;
        let bytes = self.writer_mut().finish()?;
        if let Some((at, field)) = mvhd_duration_patch(&self.header, self.video_end_us) {
            self.writer_mut().patch(at, &field)?;
        }
        Ok(bytes)
    }

    fn abandon(&mut self) {
        self.done = true;
        self.writer_mut().abandon();
    }
}

impl Drop for Fmp4Sink {
    fn drop(&mut self) {
        // SAFETY: tear down in reverse: detach our AVIO from the format context
        // (so its destructor does not `avio_close` a context it does not own),
        // free the context, then the AVIO buffer + context, then the writer.
        unsafe {
            if let Some(mut octx) = self.octx.take() {
                (*octx.as_mut_ptr()).pb = std::ptr::null_mut();
                drop(octx);
            }
            if !self.io.is_null() {
                ffi::av_freep(&mut (*self.io).buffer as *mut *mut u8 as *mut c_void);
                ffi::avio_context_free(&mut self.io);
            }
            if !self.writer.is_null() {
                let mut writer = Box::from_raw(self.writer);
                self.writer = std::ptr::null_mut();
                if !self.done {
                    writer.abandon();
                }
            }
        }
    }
}

/// Opens [`Fmp4Sink`]s with the live encoder's parameters (`tap`'s per-generation record).
pub(crate) struct Fmp4Factory;

impl SinkFactory for Fmp4Factory {
    type Sink = Fmp4Sink;
    type Video = ff::codec::Parameters;
    fn open(&mut self, part: &Path, tracks: &[TrackInfo], video: Option<&ff::codec::Parameters>) -> Result<Fmp4Sink, String> {
        let params = video.ok_or("the video encoder has not started yet")?;
        Fmp4Sink::create(part, params, tracks)
    }
    fn stream_of(&self, generation: u64) -> Option<(ff::codec::Parameters, Cause)> {
        tap::stream_of(generation)
    }
    fn newest(&self) -> (u64, Option<ff::codec::Parameters>) {
        tap::newest()
    }
}

#[cfg(test)]
#[path = "fmp4_tests.rs"]
mod tests;
