//! Where a clip's picture sits in its file: seconds to byte offsets from the video track's
//! sample tables, and the keyframe times, so a clip that is still loading can show which part
//! of the timeline is on the SSD before anything has been decoded.
use std::io::{Read, Seek, SeekFrom};

use super::mp4;

/// One map point per this many seconds of picture.
const STEP: f64 = 0.25;
/// More samples than any real clip (16 days at 60 fps): a damaged table is not walked.
const MAX_SAMPLES: u64 = 83_000_000;

/// `moov` of the file: (start of the box, end of the box).
pub fn moov_range<R: Read + Seek>(reader: &mut R) -> Option<(u64, u64)> {
    let end = reader.seek(SeekFrom::End(0)).ok()?;
    let (payload, box_end) = mp4::find_box(reader, 0, end, b"moov")?;
    Some((payload.saturating_sub(16), box_end))
}

/// Picture time to file offset for one clip.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClipMap {
    /// (seconds, byte offset of the sample starting then), ascending in time.
    pub points: Vec<(f64, u64)>,
    /// Where the track's last frame starts, and where the track ends.
    pub last_frame: Option<f64>,
    pub video_end: Option<f64>,
    /// Keyframe times in seconds (before the timeline origin is taken off).
    pub keyframes: Vec<f64>,
}

impl ClipMap {
    /// Byte offset of the picture at `seconds`.
    pub fn offset_at(&self, seconds: f64) -> u64 {
        let index = self.points.partition_point(|point| point.0 <= seconds);
        self.points[index.saturating_sub(1).min(self.points.len().saturating_sub(1))].1
    }

    /// The file range holding `[from, to)` seconds, a map step wider on each side.
    pub fn bytes_of(&self, from: f64, to: f64) -> (u64, u64) {
        if self.points.is_empty() {
            return (0, 0);
        }
        let start = self.points.partition_point(|point| point.0 <= from).saturating_sub(1);
        let end = self.points.partition_point(|point| point.0 <= to).min(self.points.len() - 1);
        (self.points[start].1, self.points[end].1 + 1)
    }

    /// Time spans whose bytes fall in `holes` (sorted `[start, end)` byte runs), merged.
    pub fn missing_spans(&self, holes: &[(u64, u64)], duration: f64) -> Vec<(f64, f64)> {
        let mut spans: Vec<(f64, f64)> = Vec::new();
        for (index, (seconds, offset)) in self.points.iter().copied().enumerate() {
            let hole = holes.partition_point(|run| run.1 <= offset);
            if holes.get(hole).is_none_or(|run| run.0 > offset) {
                continue;
            }
            let end = self.points.get(index + 1).map_or(duration, |next| next.0).min(duration);
            match spans.last_mut() {
                Some(last) if (last.1 - seconds).abs() < 1e-9 => last.1 = end,
                _ => spans.push((seconds, end)),
            }
        }
        spans
    }
}

/// Builds the map from the video track's tables. None when the clip has no sample tables
/// (a fragmented recording keeps them in its fragments).
pub fn clip_map<R: Read + Seek>(reader: &mut R) -> Option<ClipMap> {
    let (trak, mdia) = mp4::video_track(reader)?;
    let mdhd = mp4::find_box(reader, mdia.0, mdia.1, b"mdhd")?;
    let (timescale, _) = mp4::read_mvhd(reader, mdhd)?;
    if timescale == 0 {
        return None;
    }
    let minf = mp4::find_box(reader, mdia.0, mdia.1, b"minf")?;
    let stbl = mp4::find_box(reader, minf.0, minf.1, b"stbl")?;
    let stts = mp4::find_box(reader, stbl.0, stbl.1, b"stts")?;
    let stts = mp4::read_runs(reader, stts)?;
    let stsc = mp4::find_box(reader, stbl.0, stbl.1, b"stsc")?;
    let stsc = read_stsc(reader, stsc)?;
    let chunks = read_chunks(reader, stbl)?;
    let sizes = mp4::find_box(reader, stbl.0, stbl.1, b"stsz")?;
    let sizes = read_sizes(reader, sizes)?;
    let sync = mp4::find_box(reader, stbl.0, stbl.1, b"stss");
    let sync = sync.and_then(|stss| read_u32s(reader, stss));
    if stts.iter().map(|run| u64::from(run.0)).sum::<u64>() > MAX_SAMPLES {
        return None;
    }
    let shift = mp4::edit_start(reader, trak).unwrap_or(0);
    let scale = f64::from(timescale);

    let mut points: Vec<(f64, u64)> = Vec::new();
    let mut keyframes = Vec::new();
    let mut sample = 0u64;
    let mut decode = 0i64;
    let mut durations = stts.iter().copied().flat_map(|(count, delta)| std::iter::repeat_n(delta, count as usize));
    let mut next_sync = sync.as_ref().map(|list| list.iter().copied().peekable());
    let mut run = 0usize;
    for (index, chunk_offset) in chunks.iter().copied().enumerate() {
        while stsc.get(run + 1).is_some_and(|next| u64::from(next.0) <= index as u64 + 1) {
            run += 1;
        }
        let per_chunk = stsc.get(run).map_or(1, |entry| entry.1);
        let mut offset = chunk_offset;
        for _ in 0..per_chunk {
            let Some(delta) = durations.next() else { break };
            let seconds = (decode - shift).max(0) as f64 / scale;
            sample += 1;
            let is_sync = match next_sync.as_mut() {
                Some(list) => {
                    while list.peek().is_some_and(|number| u64::from(*number) < sample) {
                        list.next();
                    }
                    list.peek().is_some_and(|number| u64::from(*number) == sample)
                }
                None => true,
            };
            if is_sync {
                keyframes.push(seconds);
            }
            if points.last().is_none_or(|last| seconds - last.0 >= STEP) {
                points.push((seconds, offset));
            }
            offset += u64::from(sizes.size(sample as usize - 1));
            decode += i64::from(delta);
        }
    }
    (!points.is_empty()).then(|| ClipMap {
        points,
        last_frame: mp4::last_frame_of(reader),
        video_end: mp4::video_duration_of(reader),
        keyframes,
    })
}

/// (first chunk, samples per chunk) runs of `stsc`.
fn read_stsc<R: Read + Seek>(reader: &mut R, (start, end): (u64, u64)) -> Option<Vec<(u32, u32)>> {
    reader.seek(SeekFrom::Start(start + 4)).ok()?;
    let count = u64::from(mp4::read_u32(reader)?).min(end.saturating_sub(start + 8) / 12);
    let mut bytes = vec![0u8; count as usize * 12];
    reader.read_exact(&mut bytes).ok()?;
    let word = |at: &[u8]| u32::from_be_bytes([at[0], at[1], at[2], at[3]]);
    Some(bytes.chunks_exact(12).map(|entry| (word(entry), word(&entry[4..]))).collect())
}

/// Chunk offsets from `stco` or `co64`.
fn read_chunks<R: Read + Seek>(reader: &mut R, stbl: (u64, u64)) -> Option<Vec<u64>> {
    if let Some(stco) = mp4::find_box(reader, stbl.0, stbl.1, b"stco") {
        return Some(read_u32s(reader, stco)?.into_iter().map(u64::from).collect());
    }
    let (start, end) = mp4::find_box(reader, stbl.0, stbl.1, b"co64")?;
    reader.seek(SeekFrom::Start(start + 4)).ok()?;
    let count = u64::from(mp4::read_u32(reader)?).min(end.saturating_sub(start + 8) / 8);
    let mut bytes = vec![0u8; count as usize * 8];
    reader.read_exact(&mut bytes).ok()?;
    Some(bytes.chunks_exact(8).map(|value| u64::from_be_bytes(value.try_into().unwrap_or([0; 8]))).collect())
}

/// The u32 list of an `stco` or `stss` box.
fn read_u32s<R: Read + Seek>(reader: &mut R, (start, end): (u64, u64)) -> Option<Vec<u32>> {
    reader.seek(SeekFrom::Start(start + 4)).ok()?;
    let count = u64::from(mp4::read_u32(reader)?).min(end.saturating_sub(start + 8) / 4);
    let mut bytes = vec![0u8; count as usize * 4];
    reader.read_exact(&mut bytes).ok()?;
    Some(bytes.chunks_exact(4).map(|value| u32::from_be_bytes(value.try_into().unwrap_or([0; 4]))).collect())
}

/// `stsz`: one size for all samples, or a list.
enum Sizes {
    Fixed(u32),
    Listed(Vec<u32>),
}

impl Sizes {
    fn size(&self, sample: usize) -> u32 {
        match self {
            Sizes::Fixed(size) => *size,
            Sizes::Listed(list) => list.get(sample).copied().unwrap_or(0),
        }
    }
}

fn read_sizes<R: Read + Seek>(reader: &mut R, (start, end): (u64, u64)) -> Option<Sizes> {
    reader.seek(SeekFrom::Start(start + 4)).ok()?;
    let fixed = mp4::read_u32(reader)?;
    if fixed != 0 {
        return Some(Sizes::Fixed(fixed));
    }
    let count = u64::from(mp4::read_u32(reader)?).min(end.saturating_sub(start + 12) / 4);
    let mut bytes = vec![0u8; count as usize * 4];
    reader.read_exact(&mut bytes).ok()?;
    Some(Sizes::Listed(bytes.chunks_exact(4).map(|value| u32::from_be_bytes(value.try_into().unwrap_or([0; 4]))).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> ClipMap {
        ClipMap { points: vec![(0.0, 100), (1.0, 200), (2.0, 300), (3.0, 400)], ..Default::default() }
    }

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = (body.len() as u32 + 8).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|value| value.to_be_bytes()).collect()
    }

    /// A clip of one video track with these sample tables; 30 ticks a second.
    fn clip(stts: &[u32], stsc: &[u32], stss: &[u32]) -> std::io::Cursor<Vec<u8>> {
        let tables = [
            boxed(b"stts", &words(&[&[0, stts.len() as u32 / 2][..], stts].concat())),
            boxed(b"stsc", &words(&[&[0, stsc.len() as u32 / 3][..], stsc].concat())),
            boxed(b"stco", &words(&[0, 1, 1000])),
            boxed(b"stsz", &words(&[0, 100, 0])),
            boxed(b"stss", &words(&[&[0, stss.len() as u32][..], stss].concat())),
        ]
        .concat();
        let mdhd = boxed(b"mdhd", &words(&[0, 0, 0, 30, 3000, 0]));
        let hdlr = boxed(b"hdlr", &[&[0u8; 8][..], b"vide", &[0u8; 12]].concat());
        let minf = boxed(b"minf", &boxed(b"stbl", &tables));
        let trak = boxed(b"trak", &boxed(b"mdia", &[mdhd, hdlr, minf].concat()));
        std::io::Cursor::new(boxed(b"moov", &trak))
    }

    fn map_within(mut file: std::io::Cursor<Vec<u8>>, limit: std::time::Duration) -> Option<ClipMap> {
        let (done, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(clip_map(&mut file));
        });
        answer.recv_timeout(limit).expect("a damaged table must not hold the open")
    }

    #[test]
    fn a_small_clip_of_this_shape_maps() {
        let map = map_within(clip(&[3000, 1], &[1, 3000, 1], &[1]), std::time::Duration::from_secs(5)).expect("the shape parses");
        assert!(map.points.len() > 300, "{}", map.points.len());
        assert_eq!(map.keyframes, vec![0.0]);
    }

    #[test]
    fn huge_damaged_tables_are_refused_not_walked() {
        let huge = clip(&[u32::MAX, 1, u32::MAX, 1], &[1, u32::MAX, 1], &[1]);
        assert_eq!(map_within(huge, std::time::Duration::from_secs(10)), None);
        let just_over = clip(&[MAX_SAMPLES as u32 + 1, 1], &[1, u32::MAX, 1], &[1]);
        assert_eq!(map_within(just_over, std::time::Duration::from_secs(10)), None);
    }

    #[test]
    fn offsets_follow_the_picture() {
        assert_eq!(map().offset_at(0.0), 100);
        assert_eq!(map().offset_at(1.5), 200);
        assert_eq!(map().offset_at(99.0), 400);
    }

    #[test]
    fn a_range_of_time_covers_the_samples_around_it() {
        assert_eq!(map().bytes_of(1.5, 2.5), (200, 401));
        assert_eq!(map().bytes_of(0.0, 0.5), (100, 201));
    }

    #[test]
    fn holes_become_merged_time_spans() {
        assert_eq!(map().missing_spans(&[(0, 150), (190, 310)], 4.0), vec![(0.0, 3.0)]);
        assert_eq!(map().missing_spans(&[(0, 150), (290, 310)], 4.0), vec![(0.0, 1.0), (2.0, 3.0)]);
        assert_eq!(map().missing_spans(&[], 4.0), vec![]);
        assert_eq!(map().missing_spans(&[(0, 1000)], 3.5), vec![(0.0, 3.5)]);
    }
}
