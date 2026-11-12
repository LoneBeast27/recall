//! A bug report bundle: the person's own words, an anonymous system summary, the action
//! traces, crash records, redacted copies of the developer logs and settings, and the media
//! they added, stored in one ZIP for them to look through and attach to a GitHub issue.
//! Nothing here sends anything. [`prepare`] makes the entries (shown to the person first),
//! [`save`] writes them once they confirm.

pub mod media;
pub mod redact;
pub mod zip;

use std::fs;
use std::path::{Path, PathBuf};

pub use media::{Kind, MAX_FILE, MAX_MEDIA, MAX_REPORT};
pub use redact::Names;

/// Where reports are saved: `RECALL_REPORTS_DIR` when set, none in a test or check run (any
/// `TEST_MARKERS` variable), else `%LOCALAPPDATA%\Recall\reports`.
pub fn reports_dir() -> Option<PathBuf> {
    reports_dir_from(|k| std::env::var(k).ok())
}

pub(crate) fn reports_dir_from(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(dir) = var("RECALL_REPORTS_DIR").filter(|v| !v.is_empty()) {
        return Some(dir.into());
    }
    if crate::TEST_MARKERS.iter().any(|m| var(m).is_some_and(|v| !v.is_empty())) {
        return None;
    }
    var("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Recall").join("reports"))
}

/// What goes into one report.
#[derive(Default)]
pub struct Input {
    /// The person's description, the only free text in a report.
    pub text: String,
    /// Recall's version and the Windows build, as shown in the summary.
    pub version: String,
    pub windows: String,
    /// The trace folder (traces and crash records), when the person keeps it switched on.
    pub trace_dir: Option<PathBuf>,
    /// Developer logs and settings to include as redacted copies: (name in the report, file).
    pub logs: Vec<(String, PathBuf)>,
    /// Media the person added (each checked again here).
    pub media: Vec<PathBuf>,
}

/// The entries of a report, made but not written: shown to the person before they confirm.
pub struct Prepared {
    pub entries: Vec<zip::Entry>,
}

impl Prepared {
    /// (name in the zip, bytes) per entry.
    pub fn files(&self) -> Vec<(String, u64)> {
        self.entries.iter().map(|e| (e.name.clone(), e.data.len() as u64)).collect()
    }

    /// The zip's size on disk: the data plus every entry's two headers and the end record.
    pub fn zip_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.data.len() as u64 + 76 + 2 * e.name.len() as u64).sum::<u64>() + 22
    }
}

/// Check a file for the + button: its type by first bytes and its size.
pub fn check_media(path: &Path) -> Result<Kind, &'static str> {
    let meta = fs::metadata(path).map_err(|_| "Recall could not read this file.")?;
    if !meta.is_file() {
        return Err("Only PNG or JPEG screenshots and MP4 clips can be added.");
    }
    let mut head = [0u8; 16];
    let read = fs::File::open(path).and_then(|mut f| std::io::Read::read(&mut f, &mut head)).map_err(|_| "Recall could not read this file.")?;
    let kind = media::sniff(&head[..read]);
    match media::refusal(meta.len(), kind) {
        Some(why) => Err(why),
        None => kind.ok_or("Only PNG or JPEG screenshots and MP4 clips can be added."),
    }
}

/// The summary at the top of a report and the issue text.
pub fn summary(input: &Input) -> String {
    format!("Recall {}\nWindows {}\n", input.version, input.windows)
}

/// Make every entry of the report (nothing is written).
pub fn prepare(input: &Input, names: &Names) -> Result<Prepared, String> {
    if input.media.len() > MAX_MEDIA {
        return Err(format!("A report takes up to {MAX_MEDIA} files."));
    }
    // Sizes first, from the file system: nothing over the cap is read.
    let mut media_bytes = 0u64;
    for path in &input.media {
        check_media(path).map_err(str::to_owned)?;
        media_bytes += fs::metadata(path).map_or(0, |m| m.len());
    }
    if media_bytes > MAX_REPORT {
        return Err(too_big());
    }
    let mut entries = vec![zip::Entry { name: "report.txt".into(), data: format!("{}\n{}\n", summary(input), input.text.trim()).into_bytes() }];
    if let Some(trace) = &input.trace_dir {
        let mut found: Vec<PathBuf> = fs::read_dir(trace).into_iter().flatten().flatten().map(|e| e.path()).collect();
        found.sort();
        for path in found {
            let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
            let keep = regular_file(&path) && (name.ends_with(".trace") || name.ends_with(".trace.1") || (name.starts_with("crash-") && name.ends_with(".json")));
            // A trace is anonymous by construction; the content check still guards it.
            if let (true, Ok(text)) = (keep, fs::read_to_string(&path)) {
                if crate::find_content(&text, names.user.as_deref()).is_none() {
                    entries.push(zip::Entry { name: format!("trace/{name}"), data: text.into_bytes() });
                }
            }
        }
    }
    for (name, path) in &input.logs {
        if let Ok(bytes) = fs::read(path) {
            let text = String::from_utf8_lossy(&bytes);
            entries.push(zip::Entry { name: format!("logs/{name}"), data: redact::redact(&text, names).into_bytes() });
        }
    }
    for (i, path) in input.media.iter().enumerate() {
        let kind = check_media(path).map_err(str::to_owned)?;
        let data = fs::read(path).map_err(|_| "Recall could not read an added file.".to_string())?;
        let data = media::clean(kind, data).ok_or("An added file is damaged, or a clip carries extra data such as a GPS track. Export the clip again from Recall or an editor.")?;
        entries.push(zip::Entry { name: format!("media/media-{}.{}", i + 1, kind.ext()), data });
    }
    let prepared = Prepared { entries };
    if prepared.zip_bytes() > MAX_REPORT {
        return Err(too_big());
    }
    Ok(prepared)
}

fn too_big() -> String {
    "The report is over 24 MB, the most GitHub takes. Remove a clip or add a shorter one.".into()
}

/// Write a prepared report as `dir\report-<n>.zip` (the next free number).
pub fn save(prepared: &Prepared, dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("Recall could not make the reports folder ({e})."))?;
    let next = (1..).find(|n| !dir.join(format!("report-{n}.zip")).exists()).unwrap_or(1);
    let path = dir.join(format!("report-{next}.zip"));
    let mut file = fs::File::create(&path).map_err(|e| format!("Recall could not save the report ({e})."))?;
    zip::write(&mut file, &prepared.entries).map_err(|e| format!("Recall could not save the report ({e})."))?;
    Ok(path)
}

/// The longest new-issue address sent to the browser (browsers and GitHub take about 8 KB).
const URL_MAX: usize = 7000;

/// The new-issue page on GitHub with the title and text filled in. The person attaches the
/// zip there themselves; the address carries only what they typed and the summary. The first
/// line of the description is the title, the rest the body, cut to fit [`URL_MAX`].
pub fn issue_url(input: &Input) -> String {
    let text = input.text.trim();
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let title: String = if first.trim().is_empty() { "Bug report".into() } else { first.trim().chars().take(80).collect() };
    let base = format!("https://github.com/LoneBeast27/recall/issues/new?title={}&body=", encode(&title));
    let footer = format!("\n\n---\n{}The report zip is attached below.\n", summary(input));
    let mut body: String = rest.trim().to_string();
    loop {
        let url = format!("{base}{}", encode(&format!("{body}{footer}")));
        if url.len() <= URL_MAX || body.is_empty() {
            return url;
        }
        let keep = body.chars().count() * 3 / 4;
        body = body.chars().take(keep).collect();
    }
}

/// Percent-encoding for a URL query value.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A plain file, not a link or a junction (`symlink_metadata` does not follow them).
fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// Remove this PC's trace and crash records (the person's own data, on request). Clips and
/// settings are never touched: only plain files in `dir` itself named `*.trace`, `*.trace.1`,
/// `crash-*.json` or `crash-*.pending`; links and folders are left alone.
pub fn clear_trace(dir: &Path) -> usize {
    let mut removed = 0;
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = name.ends_with(".trace") || name.ends_with(".trace.1") || (name.starts_with("crash-") && (name.ends_with(".json") || name.ends_with(".pending")));
        if ours && regular_file(&entry.path()) && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests;
