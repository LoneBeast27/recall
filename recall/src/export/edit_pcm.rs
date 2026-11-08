//! Sample positions of the kept ranges for one audio track: where each range starts and
//! ends in the source and in the output (`pcm_stream` walks them), and track gain.

use crate::edit_model::TimeRange;

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
}
