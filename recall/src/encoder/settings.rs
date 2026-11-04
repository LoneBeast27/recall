//! Recording codec + quality knobs, read once from the environment at engine
//! start (the controller / Settings page sets them before spawning the engine).
//!
//! | Env                  | Values                                   | Default | Effect |
//! |----------------------|------------------------------------------|---------|--------|
//! | `RECALL_CODEC`       | `h264` / `hevc` (`h265`) / `av1`          | `h264`  | NVENC encoder `h264_nvenc` / `hevc_nvenc` / `av1_nvenc`. If it cannot open, recording falls back to `h264_nvenc` (logged, `codec` in status). |
//! | `RECALL_QUALITY`     | `balanced` / `high` / `max`              | `high`  | NVENC effort: preset p4 / p5 / p7 (`max` adds quarter-res multipass + spatial AQ), and the bitrate used when `RECALL_BITRATE` is unset: 15 / 25 / 40 Mbps. |
//! | `RECALL_BITRATE`     | Mbps (`25`, `12.5`, `25M`, `25mbps`), kbps (`8000k`), bps (any plain number > 1000, e.g. `25000000`) | quality default | VBR target; peak = 2x target. Clamped to 1..=500 Mbps. |
//! | `RECALL_FPS`         | integer 1..=1000                         | `60`    | Nominal rate: NVENC rate-control hint, GOP = 1 s of it (save granularity), fallback packet duration. Capture stays VFR. |
//! | `RECALL_BUFFER_SECS` | integer 5..=1800                         | `60`    | Replay retention of the video ring and every audio ring; the full-buffer save length. |
//!
//! Unparseable values (and 0) fall back to the default; out-of-range values
//! clamp to the nearest bound. Either way the encoder thread logs one note at
//! start; a bad knob never stops recording.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    /// Short name used on the wire (`status.codec`) and in env values.
    pub fn label(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
            Codec::Av1 => "av1",
        }
    }

    /// FFmpeg encoder that records this codec.
    pub fn nvenc(self) -> &'static str {
        match self {
            Codec::H264 => "h264_nvenc",
            Codec::Hevc => "hevc_nvenc",
            Codec::Av1 => "av1_nvenc",
        }
    }

    pub fn parse(value: &str) -> Option<Codec> {
        match value.trim().to_ascii_lowercase().as_str() {
            "h264" | "h.264" | "avc" => Some(Codec::H264),
            "hevc" | "h265" | "h.265" => Some(Codec::Hevc),
            "av1" => Some(Codec::Av1),
            _ => None,
        }
    }

    /// Stable index for an `AtomicU64` (0 is never produced).
    pub fn index(self) -> u64 {
        match self {
            Codec::H264 => 1,
            Codec::Hevc => 2,
            Codec::Av1 => 3,
        }
    }

    pub fn from_index(index: u64) -> Codec {
        match index {
            2 => Codec::Hevc,
            3 => Codec::Av1,
            _ => Codec::H264,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Balanced,
    High,
    Max,
}

impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Quality::Balanced => "balanced",
            Quality::High => "high",
            Quality::Max => "max",
        }
    }

    pub fn parse(value: &str) -> Option<Quality> {
        match value.trim().to_ascii_lowercase().as_str() {
            "balanced" | "medium" => Some(Quality::Balanced),
            "high" => Some(Quality::High),
            "max" | "maximum" => Some(Quality::Max),
            _ => None,
        }
    }

    /// Target bitrate when `RECALL_BITRATE` is not set.
    pub fn default_bitrate(self) -> usize {
        match self {
            Quality::Balanced => 15_000_000,
            Quality::High => 25_000_000,
            Quality::Max => 40_000_000,
        }
    }

    /// The effort a low-end NVIDIA GPU (GTX 10/16, MX: one older NVENC unit) holds at 60
    /// fps next to a game: High and Max step down to Balanced's p4 without multipass. The
    /// bitrate is unchanged (it comes from `RECALL_BITRATE` or the requested quality).
    pub fn for_gpu(self, low_end: bool) -> Quality {
        if low_end {
            Quality::Balanced
        } else {
            self
        }
    }

    /// NVENC private options. `bf=0` is mandatory: the ring is muxed with
    /// dts == pts, which only holds without B-frames.
    pub fn nvenc_options(self) -> Vec<(&'static str, &'static str)> {
        let preset = match self {
            Quality::Balanced => "p4",
            Quality::High => "p5",
            Quality::Max => "p7",
        };
        let mut opts = vec![("preset", preset), ("tune", "hq"), ("rc", "vbr"), ("bf", "0")];
        if self == Quality::Max {
            opts.push(("multipass", "qres"));
            opts.push(("spatial-aq", "1"));
        }
        opts
    }
}

/// Encoders to try, in order, for a requested codec: itself, then H.264.
pub fn fallback_chain(requested: Codec) -> Vec<Codec> {
    let mut chain = vec![requested];
    if requested != Codec::H264 {
        chain.push(Codec::H264);
    }
    chain
}

/// An encoder opened by `open_with_fallback`.
pub struct Opened<T> {
    pub value: T,
    pub codec: Codec,
    /// Set when `codec` is not the requested one: the line to log.
    pub fallback: Option<String>,
}

/// Open the requested codec, else fall back along `fallback_chain`.
pub fn open_with_fallback<T>(
    requested: Codec,
    mut open: impl FnMut(Codec) -> Result<T, String>,
) -> Result<Opened<T>, String> {
    let mut failures = Vec::new();
    for codec in fallback_chain(requested) {
        match open(codec) {
            Ok(value) => {
                let fallback = (!failures.is_empty()).then(|| {
                    format!(
                        "{} unavailable ({}); recording with {} instead",
                        requested.nvenc(),
                        failures.join("; "),
                        codec.nvenc()
                    )
                });
                return Ok(Opened {
                    value,
                    codec,
                    fallback,
                });
            }
            Err(e) => failures.push(format!("{}: {e}", codec.nvenc())),
        }
    }
    Err(format!("NVENC initialization failed: {}", failures.join("; ")))
}

/// Encoder thread configuration (`encoder::Config`).
pub struct Config {
    pub fps: u32,
    pub buffer_secs: u32,
    pub bitrate: usize,
    /// Requested codec; `h264_nvenc` is the fallback when it cannot open.
    pub codec: Codec,
    pub quality: Quality,
    /// Emit machine JSON events (Saved/SaveFailed) instead of human logs.
    pub json: bool,
    /// Env parse notes, logged once by the encoder thread.
    pub notes: Vec<String>,
}

impl Config {
    /// `RECALL_CODEC/_QUALITY/_BITRATE/_FPS/_BUFFER_SECS` (table above).
    pub fn from_env(json: bool) -> Self {
        let k = from_env();
        Config {
            fps: k.fps,
            buffer_secs: k.buffer_secs,
            bitrate: k.bitrate,
            codec: k.codec,
            quality: k.quality,
            json,
            notes: k.notes,
        }
    }

    /// One log line naming every effective encoder setting.
    pub fn summary(&self) -> String {
        let opts: Vec<String> = self
            .quality
            .nvenc_options()
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        format!(
            "encoder config: codec={} ({}) quality={} [{}] bitrate={} peak={} gop={} buffer={}s",
            self.codec.label(),
            self.codec.nvenc(),
            self.quality.label(),
            opts.join(" "),
            mbps(self.bitrate),
            mbps(self.bitrate * 2),
            self.fps,
            self.buffer_secs
        )
    }
}

/// Parsed knobs plus notes about any value that was rejected or clamped.
#[derive(Clone, Debug, PartialEq)]
pub struct Knobs {
    pub codec: Codec,
    pub quality: Quality,
    pub fps: u32,
    pub bitrate: usize,
    pub buffer_secs: u32,
    pub notes: Vec<String>,
}

pub const DEFAULT_FPS: u32 = 60;
pub const DEFAULT_BUFFER_SECS: u32 = 60;
const MIN_BITRATE: usize = 1_000_000;
const MAX_BITRATE: usize = 500_000_000;

pub fn from_env() -> Knobs {
    from_lookup(|key| std::env::var(key).ok())
}

/// `from_env` with an injectable lookup (tests).
pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Knobs {
    let mut notes = Vec::new();
    let codec = parse_or(&get, "RECALL_CODEC", Codec::parse, Codec::H264, &mut notes);
    let quality = parse_or(&get, "RECALL_QUALITY", Quality::parse, Quality::High, &mut notes);
    let fps = int_in_range(&get, "RECALL_FPS", 1, 1000, DEFAULT_FPS, &mut notes);
    let buffer_secs = int_in_range(&get, "RECALL_BUFFER_SECS", 5, 1800, DEFAULT_BUFFER_SECS, &mut notes);
    let bitrate = match get("RECALL_BITRATE").filter(|v| !v.trim().is_empty()) {
        None => quality.default_bitrate(),
        Some(raw) => match parse_bitrate(&raw) {
            Some(bps) if (MIN_BITRATE..=MAX_BITRATE).contains(&bps) => bps,
            Some(bps) => {
                let clamped = bps.clamp(MIN_BITRATE, MAX_BITRATE);
                notes.push(format!(
                    "RECALL_BITRATE {raw:?} is outside 1..=500 Mbps; using {}",
                    mbps(clamped)
                ));
                clamped
            }
            None => {
                let fallback = quality.default_bitrate();
                notes.push(format!(
                    "RECALL_BITRATE {raw:?} is not a bitrate (try 25, 25M or 25000000); using {}",
                    mbps(fallback)
                ));
                fallback
            }
        },
    };
    Knobs {
        codec,
        quality,
        fps,
        bitrate,
        buffer_secs,
        notes,
    }
}

/// Bits per second from `25`, `12.5`, `25M`, `25 Mbps`, `8000k`, `25000000`.
/// A plain number up to 1000 is Mbps; above that it is bps.
pub fn parse_bitrate(raw: &str) -> Option<usize> {
    let text = raw.trim().to_ascii_lowercase();
    let text = text.strip_suffix("bps").unwrap_or(&text).trim_end();
    let (number, scale) = if let Some(n) = text.strip_suffix('m') {
        (n, Some(1e6))
    } else if let Some(n) = text.strip_suffix('k') {
        (n, Some(1e3))
    } else {
        (text, None)
    };
    let value: f64 = number.trim().parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let scale = scale.unwrap_or(if value <= 1000.0 { 1e6 } else { 1.0 });
    Some((value * scale).round() as usize)
}

pub fn mbps(bps: usize) -> String {
    format!("{:.1} Mbps", bps as f64 / 1e6)
}

fn parse_or<T: Copy>(
    get: &impl Fn(&str) -> Option<String>,
    key: &str,
    parse: impl Fn(&str) -> Option<T>,
    default: T,
    notes: &mut Vec<String>,
) -> T {
    match get(key).filter(|v| !v.trim().is_empty()) {
        None => default,
        Some(raw) => parse(&raw).unwrap_or_else(|| {
            notes.push(format!("{key} {raw:?} is not recognised; using the default"));
            default
        }),
    }
}

fn int_in_range(
    get: &impl Fn(&str) -> Option<String>,
    key: &str,
    min: u32,
    max: u32,
    default: u32,
    notes: &mut Vec<String>,
) -> u32 {
    let Some(raw) = get(key).filter(|v| !v.trim().is_empty()) else {
        return default;
    };
    match raw.trim().parse::<u64>() {
        // 0 reads as "unset", not "as low as possible" (1 fps = all-intra GOP).
        Ok(0) => {
            notes.push(format!("{key} {raw:?} must be positive; using {default}"));
            default
        }
        Ok(v) if (min as u64..=max as u64).contains(&v) => v as u32,
        Ok(v) => {
            let clamped = v.clamp(min as u64, max as u64) as u32;
            notes.push(format!("{key} {raw:?} is outside {min}..={max}; using {clamped}"));
            clamped
        }
        Err(_) => {
            notes.push(format!("{key} {raw:?} is not a whole number; using {default}"));
            default
        }
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
