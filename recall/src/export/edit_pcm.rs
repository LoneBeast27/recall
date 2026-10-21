//! Edited PCM timeline for one audio track: decoded source frames are placed
//! by sample position into the kept ranges. Normal-speed ranges are written
//! straight into the output; ranges with a speed change are collected at
//! source length and time-stretched into place once decoding has finished.

use ffmpeg_next as ff;

use crate::edit_model::TimeRange;

/// Edited PCM is retained only until it is fed to AAC. Bound it so a malformed
/// or unusually long project returns an error instead of risking process abort.
pub(crate) const MAX_EDITED_PCM_BYTES_PER_TRACK: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SampleRange {
    pub source_start: i64,
    pub source_end: i64,
    pub output_start: usize,
    pub output_end: usize,
    pub speed: f64,
}

pub(crate) fn range_sample_bounds(ranges: &[TimeRange], rate: u32) -> Result<Vec<SampleRange>, String> {
    let mut output_start = 0usize;
    ranges
        .iter()
        .map(|range| {
            let source_start = seconds_to_samples(range.start, rate)?;
            let source_end = seconds_to_samples(range.end, rate)?;
            let count = source_end.saturating_sub(source_start) as f64 / range.speed;
            if !count.is_finite() || count > (usize::MAX / 2) as f64 {
                return Err("edited audio range is too large".to_string());
            }
            let output_end = output_start
                .checked_add(count.round() as usize)
                .ok_or_else(|| "edited audio duration is too large".to_string())?;
            let bound = SampleRange {
                source_start,
                source_end,
                output_start,
                output_end,
                speed: range.speed,
            };
            output_start = output_end;
            Ok(bound)
        })
        .collect()
}

fn seconds_to_samples(seconds: f64, rate: u32) -> Result<i64, String> {
    let value = seconds * rate as f64;
    if !value.is_finite() || value < 0.0 || value > i64::MAX as f64 {
        return Err("edited audio time is outside the supported range".into());
    }
    Ok(value.round() as i64)
}

pub(crate) fn gain_multiplier(gain_db: f64, muted: bool) -> f32 {
    if muted {
        0.0
    } else {
        10.0f64.powf(gain_db / 20.0) as f32
    }
}

pub(crate) fn pcm_allocation_bytes(channels: usize, total_samples: usize) -> Option<usize> {
    total_samples
        .checked_mul(channels)
        .and_then(|samples| samples.checked_mul(std::mem::size_of::<f32>()))
}

fn allocate_pcm(channels: usize, samples: usize, stream_index: usize) -> Result<Vec<Vec<f32>>, String> {
    let mut pcm = Vec::new();
    pcm.try_reserve_exact(channels).map_err(|error| {
        format!("reserve edited PCM channels for audio stream {stream_index}: {error}")
    })?;
    for _ in 0..channels {
        let mut channel = Vec::new();
        channel.try_reserve_exact(samples).map_err(|error| {
            format!(
                "reserve {} MiB edited PCM for audio stream {stream_index}: {error}",
                samples
                    .saturating_mul(std::mem::size_of::<f32>())
                    .div_ceil(1024 * 1024)
            )
        })?;
        channel.resize(samples, 0.0);
        pcm.push(channel);
    }
    Ok(pcm)
}

pub(crate) struct EditedPcm {
    bounds: Vec<SampleRange>,
    output: Vec<Vec<f32>>,
    /// Source-length buffers for ranges whose speed is not 1.0.
    staging: Vec<Option<Vec<Vec<f32>>>>,
    gain: f32,
}

impl EditedPcm {
    pub fn new(
        ranges: &[TimeRange],
        rate: u32,
        channels: usize,
        gain: f32,
        stream_index: usize,
    ) -> Result<Self, String> {
        let bounds = range_sample_bounds(ranges, rate)?;
        let total = bounds.last().map_or(0, |bound| bound.output_end);
        let staged: usize = bounds
            .iter()
            .filter(|bound| bound.speed != 1.0)
            .map(|bound| (bound.source_end - bound.source_start).max(0) as usize)
            .sum();
        let allocation = pcm_allocation_bytes(channels, total.saturating_add(staged))
            .ok_or_else(|| format!("edited audio stream {stream_index} is too large"))?;
        if allocation > MAX_EDITED_PCM_BYTES_PER_TRACK {
            return Err(format!(
                "edited audio stream {stream_index} requires {} MiB of PCM; the single-clip editor limit is 256 MiB per track",
                allocation.div_ceil(1024 * 1024)
            ));
        }
        let output = allocate_pcm(channels, total, stream_index)?;
        let staging = bounds
            .iter()
            .map(|bound| {
                (bound.speed != 1.0)
                    .then(|| {
                        let length = (bound.source_end - bound.source_start).max(0) as usize;
                        allocate_pcm(channels, length, stream_index)
                    })
                    .transpose()
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            bounds,
            output,
            staging,
            gain,
        })
    }

    /// Copy the parts of a planar f32 frame that start at source sample
    /// `source_start` and fall inside kept ranges.
    pub fn place(&mut self, frame: &ff::frame::Audio, source_start: i64) {
        let source_end = source_start.saturating_add(frame.samples() as i64);
        for (index, range) in self.bounds.iter().enumerate() {
            let copy_start = source_start.max(range.source_start);
            let copy_end = source_end.min(range.source_end);
            if copy_end <= copy_start {
                continue;
            }
            let frame_offset = (copy_start - source_start) as usize;
            let range_offset = (copy_start - range.source_start) as usize;
            let count = (copy_end - copy_start) as usize;
            let (destination, offset) = match self.staging[index].as_mut() {
                Some(staged) => (staged, range_offset),
                None => (&mut self.output, range.output_start + range_offset),
            };
            for (channel, target) in destination.iter_mut().enumerate() {
                let source = frame.plane::<f32>(channel);
                let target = &mut target[offset..offset + count];
                for (out, sample) in target.iter_mut().zip(&source[frame_offset..frame_offset + count]) {
                    *out = sample * self.gain;
                }
            }
        }
    }

    /// Time-stretch the staged ranges into place and return the output PCM.
    pub fn finish(mut self, rate: u32, layout: ff::ChannelLayout) -> Result<Vec<Vec<f32>>, String> {
        for (index, staged) in self.staging.iter_mut().enumerate() {
            let Some(staged) = staged.take() else {
                continue;
            };
            let range = self.bounds[index];
            let stretched = super::tempo::stretch(&staged, rate, layout, range.speed)?;
            let length = range.output_end - range.output_start;
            for (target, source) in self.output.iter_mut().zip(&stretched) {
                let count = length.min(source.len());
                target[range.output_start..range.output_start + count].copy_from_slice(&source[..count]);
            }
        }
        Ok(self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_samples_concatenate_without_a_gap() {
        let bounds = range_sample_bounds(&[TimeRange::new(1.0, 1.5), TimeRange::new(3.0, 3.25)], 48_000).unwrap();
        assert_eq!(bounds[0].source_start, 48_000);
        assert_eq!(bounds[0].output_end, 24_000);
        assert_eq!(bounds[1].output_start, 24_000);
        assert_eq!(bounds[1].output_end, 36_000);
    }

    #[test]
    fn speed_scales_output_sample_counts() {
        let ranges = [
            TimeRange { start: 0.0, end: 1.0, speed: 0.5 },
            TimeRange { start: 2.0, end: 3.0, speed: 2.0 },
        ];
        let bounds = range_sample_bounds(&ranges, 48_000).unwrap();
        assert_eq!(bounds[0].output_end, 96_000);
        assert_eq!(bounds[1].output_end, 120_000);
    }

    #[test]
    fn mute_and_decibel_gain_are_linear() {
        assert_eq!(gain_multiplier(12.0, true), 0.0);
        assert!((gain_multiplier(-6.0, false) - 0.501_187_2).abs() < 0.000_001);
        assert!((gain_multiplier(12.0, false) - 3.981_071_7).abs() < 0.000_001);
    }

    #[test]
    fn pcm_allocation_is_bounded_and_overflow_checked() {
        assert_eq!(pcm_allocation_bytes(2, 48_000 * 60), Some(23_040_000));
        assert!(pcm_allocation_bytes(usize::MAX, 2).is_none());
        assert!(pcm_allocation_bytes(2, 48_000 * 700).unwrap() > MAX_EDITED_PCM_BYTES_PER_TRACK);
    }
}
