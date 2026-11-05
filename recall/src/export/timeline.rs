//! Time math shared by the exports: CFR tick counting, the source timeline
//! origin/duration, and the edited (speed-aware) output timeline.

use ff::Rescale;
use ffmpeg_next as ff;

use crate::edit_model::EditPlan;

/// Number of CFR ticks strictly before `time_us`.
///
/// This is `ceil(time * fps)`, expressed with integers so high-rate exports do
/// not accumulate floating-point drift.
pub(crate) fn ticks_before(time_us: i64, fps: u32) -> u64 {
    if time_us <= 0 || fps == 0 {
        return 0;
    }
    let scaled = time_us as i128 * fps as i128;
    ((scaled + 999_999) / 1_000_000) as u64
}

pub(crate) fn seconds_to_us(seconds: f64) -> Result<i64, String> {
    let value = seconds * 1_000_000.0;
    if !value.is_finite() || value < 0.0 || value > i64::MAX as f64 {
        return Err("edited video time is outside the supported range".into());
    }
    Ok(value.round() as i64)
}

pub(crate) fn us_to_seconds(us: i64) -> f64 {
    us as f64 / 1_000_000.0
}

#[cfg(test)]
thread_local! {
    /// Tests turn this off to get the old decode-the-whole-clip reference.
    pub(crate) static TRIM_DECODE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Exports seek to the first kept range and stop reading after the last one.
pub(crate) fn trim_decode() -> bool {
    #[cfg(test)]
    return TRIM_DECODE.with(std::cell::Cell::get);
    #[cfg(not(test))]
    true
}

/// Lead before a range start, so the demuxer lands on a keyframe shown before
/// it even with reordered frames, and audio decoders settle before it.
pub(crate) const SEEK_MARGIN_US: i64 = 1_000_000;

/// Seek `ictx` to the keyframe at or before `start_us` (editor time) less
/// [`SEEK_MARGIN_US`]. A failed seek leaves reading at the start of the file.
pub(crate) fn seek_near(ictx: &mut ff::format::context::Input, origin_us: i64, start_us: i64) {
    let start = start_us.saturating_sub(SEEK_MARGIN_US);
    if start <= 0 {
        return;
    }
    let target = origin_us.saturating_add(start);
    if ictx.seek(target, ..target).is_err() {
        let _ = ictx.seek(origin_us, ..origin_us);
    }
}

/// Earliest start time over the video and audio streams, in microseconds. The
/// editor timeline (and every plan time) is relative to this origin.
pub(crate) fn timeline_origin_us(ictx: &ff::format::context::Input, video_index: usize) -> i64 {
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

/// Longest video/audio stream end relative to `origin_us`, in microseconds.
pub(crate) fn timeline_duration_us(ictx: &ff::format::context::Input, origin_us: i64) -> i64 {
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

/// One kept range on both timelines, in microseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EditRangeUs {
    pub source_start: i64,
    pub source_end: i64,
    pub output_start: i64,
    pub output_end: i64,
    pub speed: f64,
}

impl EditRangeUs {
    /// Output time of a source instant inside this range (speed applied).
    pub fn output_time(&self, source_us: i64) -> i64 {
        let offset = source_us.saturating_sub(self.source_start).max(0);
        let scaled = if self.speed == 1.0 {
            offset
        } else {
            (offset as f64 / self.speed).round() as i64
        };
        self.output_start.saturating_add(scaled)
    }
}

pub(crate) struct EditTimeline {
    pub ranges: Vec<EditRangeUs>,
}

impl EditTimeline {
    pub fn from_plan(plan: &EditPlan) -> Result<Self, String> {
        let mut output_start = 0i64;
        let mut ranges = Vec::with_capacity(plan.ranges.len());
        for range in &plan.ranges {
            let source_start = seconds_to_us(range.start)?;
            let source_end = seconds_to_us(range.end)?;
            let length = source_end.saturating_sub(source_start) as f64 / range.speed;
            if !length.is_finite() || length > i64::MAX as f64 / 2.0 {
                return Err("edited video duration is too large".into());
            }
            let output_end = output_start
                .checked_add(length.round() as i64)
                .ok_or_else(|| "edited video duration is too large".to_string())?;
            ranges.push(EditRangeUs {
                source_start,
                source_end,
                output_start,
                output_end,
                speed: range.speed,
            });
            output_start = output_end;
        }
        Ok(Self { ranges })
    }

    /// Output length in microseconds.
    pub fn output_end(&self) -> i64 {
        self.ranges.last().map_or(0, |r| r.output_end)
    }
}

#[cfg(test)]
mod tests {
    use super::{ticks_before, EditTimeline};
    use crate::edit_model::{EditPlan, TimeRange};

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

    #[test]
    fn speed_scales_output_lengths_and_frame_times() {
        let plan = EditPlan { mix_down: None,
            ranges: vec![
                TimeRange { start: 1.0, end: 2.0, speed: 0.25 },
                TimeRange { start: 3.0, end: 5.0, speed: 4.0 },
            ],
            audio: vec![],
        };
        let timeline = EditTimeline::from_plan(&plan).unwrap();
        assert_eq!(timeline.ranges[0].output_end, 4_000_000);
        assert_eq!(timeline.ranges[1].output_start, 4_000_000);
        assert_eq!(timeline.output_end(), 4_500_000);
        assert_eq!(timeline.ranges[0].output_time(1_500_000), 2_000_000);
        assert_eq!(timeline.ranges[1].output_time(4_000_000), 4_250_000);
    }
}
