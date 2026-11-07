//! Undecodable audio packets. The ffmpeg CLI logs one and carries on; the editor stems and the
//! edited export do the same (that packet's time stays silent: [`BadPackets::take_gap`] moves the
//! next frame on by the skipped duration, else the rest of the track would play that much early),
//! and give up only when the decoder rejects many packets in a row: then the stream is broken,
//! not one packet.

use ffmpeg_next as ff;

/// Packets rejected back to back before the stream counts as broken.
const BROKEN_AFTER: u32 = 50;

#[derive(Default)]
pub(crate) struct BadPackets {
    in_a_row: u32,
    total: u64,
    /// Duration of the packets skipped since the last [`Self::take_gap`], in the stream's time base.
    gap: i64,
}

impl BadPackets {
    /// Sends `packet` to the decoder; one it rejects is counted and skipped.
    pub(crate) fn send(&mut self, decoder: &mut ff::decoder::Audio, packet: &ff::Packet, what: &str) -> Result<(), String> {
        match decoder.send_packet(packet) {
            Ok(()) => {
                self.in_a_row = 0;
                Ok(())
            }
            Err(error) => {
                self.gap = self.gap.saturating_add(packet.duration().max(0));
                self.skip(error, what)
            }
        }
    }

    /// Time of the packets skipped since the last call (stream time base); 0 when none.
    pub(crate) fn take_gap(&mut self) -> i64 {
        std::mem::take(&mut self.gap)
    }

    /// A decode error (from sending a packet or receiving its frame): Err once too many came in
    /// a row.
    pub(crate) fn skip(&mut self, error: ff::Error, what: &str) -> Result<(), String> {
        self.in_a_row += 1;
        self.total += 1;
        if self.in_a_row >= BROKEN_AFTER {
            return Err(format!("{what}: {error} ({} undecodable packets in a row)", self.in_a_row));
        }
        Ok(())
    }

    /// One line on stderr when packets were skipped.
    pub(crate) fn report(&self, what: &str) {
        if self.total > 0 {
            eprintln!("{what}: skipped {} undecodable audio packet(s); their time is silent", self.total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_bad_packets_are_skipped_and_a_broken_stream_still_fails() {
        let mut bad = BadPackets::default();
        for _ in 0..BROKEN_AFTER - 1 {
            assert!(bad.skip(ff::Error::InvalidData, "decode").is_ok());
        }
        assert!(bad.skip(ff::Error::InvalidData, "decode").unwrap_err().contains("50 undecodable packets in a row"));
        let mut spaced = BadPackets::default();
        for _ in 0..200 {
            assert!(spaced.skip(ff::Error::InvalidData, "decode").is_ok());
            spaced.in_a_row = 0; // a good packet between
        }
        assert_eq!(spaced.total, 200);
    }
}
