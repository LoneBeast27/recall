//! Pitch-preserving audio time-stretch for slow/fast ranges, using FFmpeg's
//! `atempo` (WSOLA). One `atempo` instance is kept within 0.5–2.0, where it
//! sounds best, by chaining instances for 0.25–0.5 and 2.0–4.0.

use ffmpeg_next as ff;

const CHUNK: usize = 4096;

/// `atempo` factors whose product is `speed`, each within 0.5–2.0.
pub(crate) fn atempo_chain(speed: f64) -> Vec<f64> {
    let mut remaining = speed;
    let mut chain = Vec::new();
    while remaining < 0.5 - 1e-9 {
        chain.push(0.5);
        remaining /= 0.5;
    }
    while remaining > 2.0 + 1e-9 {
        chain.push(2.0);
        remaining /= 2.0;
    }
    if (remaining - 1.0).abs() > 1e-9 || chain.is_empty() {
        chain.push(remaining);
    }
    chain
}

fn layout_name(layout: ff::ChannelLayout) -> String {
    let mut buffer = [0 as std::os::raw::c_char; 128];
    let written = unsafe {
        ff::ffi::av_channel_layout_describe(&layout.0, buffer.as_mut_ptr(), buffer.len())
    };
    if written <= 0 {
        return format!("{}c", layout.channels().max(1));
    }
    unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// Stretch planar f32 PCM so it lasts `1/speed` as long, keeping pitch. The
/// result is exactly `round(len / speed)` samples per channel (the filter's
/// own length can differ by a few samples; it is trimmed or zero-padded).
pub(crate) fn stretch(
    pcm: &[Vec<f32>],
    rate: u32,
    layout: ff::ChannelLayout,
    speed: f64,
) -> Result<Vec<Vec<f32>>, String> {
    let input_len = pcm.first().map_or(0, Vec::len);
    let target = (input_len as f64 / speed).round() as usize;
    let mut output: Vec<Vec<f32>> = (0..pcm.len()).map(|_| Vec::with_capacity(target)).collect();
    if input_len > 0 {
        let mut graph = build_graph(rate, layout, speed)?;
        let format = ff::format::Sample::F32(ff::format::sample::Type::Planar);
        let mut position = 0usize;
        while position < input_len {
            let count = CHUNK.min(input_len - position);
            let mut frame = ff::frame::Audio::new(format, count, layout);
            frame.set_rate(rate);
            frame.set_pts(Some(position as i64));
            for (channel, source) in pcm.iter().enumerate() {
                frame.plane_mut::<f32>(channel)[..count]
                    .copy_from_slice(&source[position..position + count]);
            }
            source(&mut graph)?
                .source()
                .add(&frame)
                .map_err(|e| format!("feed audio time-stretch: {e}"))?;
            drain(&mut graph, &mut output)?;
            position += count;
        }
        source(&mut graph)?
            .source()
            .flush()
            .map_err(|e| format!("flush audio time-stretch: {e}"))?;
        drain(&mut graph, &mut output)?;
    }
    for channel in &mut output {
        channel.resize(target, 0.0);
    }
    Ok(output)
}

fn build_graph(rate: u32, layout: ff::ChannelLayout, speed: f64) -> Result<ff::filter::Graph, String> {
    let filter = |name: &str| {
        ff::filter::find(name).ok_or_else(|| format!("FFmpeg filter {name} is unavailable"))
    };
    let mut graph = ff::filter::Graph::new();
    let args = format!(
        "time_base=1/{rate}:sample_rate={rate}:sample_fmt=fltp:channel_layout={}",
        layout_name(layout)
    );
    graph
        .add(&filter("abuffer")?, "in", &args)
        .map_err(|e| format!("create audio time-stretch input ({args}): {e}"))?;
    let mut sink = graph
        .add(&filter("abuffersink")?, "out", "")
        .map_err(|e| format!("create audio time-stretch output: {e}"))?;
    sink.set_sample_format(ff::format::Sample::F32(ff::format::sample::Type::Planar));
    sink.set_sample_rate(rate);
    sink.set_channel_layout(layout);
    let spec = atempo_chain(speed)
        .iter()
        .map(|factor| format!("atempo={factor}"))
        .collect::<Vec<_>>()
        .join(",");
    graph
        .output("in", 0)
        .and_then(|parser| parser.input("out", 0))
        .and_then(|parser| parser.parse(&spec))
        .map_err(|e| format!("build audio time-stretch {spec}: {e}"))?;
    graph
        .validate()
        .map_err(|e| format!("configure audio time-stretch {spec}: {e}"))?;
    Ok(graph)
}

fn source(graph: &mut ff::filter::Graph) -> Result<ff::filter::Context, String> {
    graph.get("in").ok_or_else(|| "audio time-stretch input vanished".to_string())
}

fn drain(graph: &mut ff::filter::Graph, output: &mut [Vec<f32>]) -> Result<(), String> {
    let mut sink = graph
        .get("out")
        .ok_or_else(|| "audio time-stretch output vanished".to_string())?;
    loop {
        let mut frame = ff::frame::Audio::empty();
        match sink.sink().frame(&mut frame) {
            Ok(()) => {
                let samples = frame.samples();
                for (channel, destination) in output.iter_mut().enumerate() {
                    destination.extend_from_slice(&frame.plane::<f32>(channel)[..samples]);
                }
            }
            Err(error) if super::receive_is_drained(&error) => return Ok(()),
            Err(error) => return Err(format!("read audio time-stretch: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains_stay_within_the_clean_atempo_range() {
        assert_eq!(atempo_chain(0.25), vec![0.5, 0.5]);
        assert_eq!(atempo_chain(4.0), vec![2.0, 2.0]);
        assert_eq!(atempo_chain(0.5), vec![0.5]);
        assert_eq!(atempo_chain(1.5), vec![1.5]);
        let chain = atempo_chain(0.3);
        assert_eq!(chain[0], 0.5);
        assert!((chain.iter().product::<f64>() - 0.3).abs() < 1e-12);
        assert!(chain.iter().all(|f| (0.5..=2.0).contains(f)));
    }

    /// Zero crossings per second of the first channel ≈ 2 × frequency.
    fn crossings_per_second(samples: &[f32], rate: u32) -> f64 {
        let crossings = samples
            .windows(2)
            .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
            .count();
        crossings as f64 * rate as f64 / samples.len() as f64
    }

    #[test]
    fn stretch_changes_length_but_keeps_pitch() {
        ff::init().unwrap();
        let rate = 48_000u32;
        let tone: Vec<f32> = (0..rate as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5)
            .collect();
        let pcm = vec![tone.clone(), tone];
        for speed in [0.25, 0.5, 2.0, 4.0] {
            let out = stretch(&pcm, rate, ff::ChannelLayout::STEREO, speed).unwrap();
            assert_eq!(out[0].len(), (rate as f64 / speed).round() as usize);
            // Skip the filter's ramp-in/out and judge the steady middle.
            let middle = &out[0][out[0].len() / 8..out[0].len() * 7 / 8];
            let hz = crossings_per_second(middle, rate) / 2.0;
            assert!((hz - 440.0).abs() < 440.0 * 0.03, "speed {speed}: pitch {hz:.1} Hz");
            let energy = middle.iter().map(|s| s * s).sum::<f32>() / middle.len() as f32;
            assert!(energy > 0.05, "speed {speed}: stretched audio is near silent ({energy})");
        }
    }
}
