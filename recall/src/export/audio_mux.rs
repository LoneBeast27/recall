//! Interleaves pre-encoded AAC tracks with a video stream being written.

use ffmpeg_next as ff;

use crate::edit_audio::EncodedAudioTrack;

pub(super) struct EditedAudioMux<'a> {
    tracks: &'a [EncodedAudioTrack],
    output_indices: Vec<usize>,
    cursors: Vec<usize>,
}

impl<'a> EditedAudioMux<'a> {
    /// Packets are copied on write, so the same tracks can be muxed again by a
    /// later attempt (size-cap retries).
    pub fn new(tracks: &'a [EncodedAudioTrack], output_indices: Vec<usize>) -> Self {
        let cursors = vec![0; tracks.len()];
        Self {
            tracks,
            output_indices,
            cursors,
        }
    }

    /// Write every queued audio packet whose timestamp is at or before
    /// `limit_seconds`, oldest first across tracks.
    pub fn write_before(
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

    pub fn finish(&mut self, output: &mut ff::format::context::Output) -> Result<(), String> {
        self.write_before(output, f64::INFINITY)
    }

    fn write_one(
        &mut self,
        output: &mut ff::format::context::Output,
        track_index: usize,
    ) -> Result<(), String> {
        let track = &self.tracks[track_index];
        let mut packet = track.packets[self.cursors[track_index]].clone();
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

/// Total payload bytes and packet count of encoded tracks.
pub(super) fn payload_totals(tracks: &[EncodedAudioTrack]) -> (u64, u64) {
    tracks.iter().flat_map(|t| t.packets.iter()).fold((0, 0), |(bytes, count), packet| {
        (bytes + packet.size() as u64, count + 1)
    })
}
