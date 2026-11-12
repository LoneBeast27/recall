//! A bug report bundle: the person's own words, an anonymous system summary, the action
//! traces, crash records, redacted copies of the developer logs and settings, and the media
//! they added, stored in one ZIP for them to look through and attach to a GitHub issue.
//! Nothing here sends anything.

pub mod media;
pub mod redact;
pub mod zip;

use std::fs;
use std::path::{Path, PathBuf};

pub use media::{Kind, MAX_FILE, MAX_REPORT};
pub use redact::Names;

/// Where reports are saved: `%LOCALAPPDATA%\Recall\reports`.
pub fn reports_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Recall").join("reports"))
}

/// What goes into one report.
#[derive(Default)]
pub struct Input {
    /// The person's description, the only free text in a report.
    pub text: String,
    /// Recall's version and the Windows build, as shown in the summary.
    pub version: String,
    pub windows: String,
    /// The trace folder (traces and crash records), when the person keeps it ticked.
    pub trace_dir: Option<PathBuf>,
    /// Developer logs and settings files to include as redacted copies: (name in the
    /// report, file).
    pub logs: Vec<(String, PathBuf)>,
    /// Media the person added (already checked with [`check_media`]).
    pub media: Vec<PathBuf>,
}

/// What was stored, for the form.
#[derive(Debug, Default, PartialEq)]
pub struct Built {
    pub path: PathBuf,
    pub files: Vec<(String, u64)>,
    pub bytes: u64,
}

/// Check a file for the + button: its type by first bytes and its size.
pub fn check_media(path: &Path) -> Result<Kind, &'static str> {
    let len = fs::metadata(path).map_err(|_| "Recall could not read this file.")?.len();
    let mut head = [0u8; 16];
    let read = fs::File::open(path).and_then(|mut f| std::io::Read::read(&mut f, &mut head)).map_err(|_| "Recall could not read this file.")?;
    let kind = media::sniff(&head[..read]);
    match media::refusal(len, kind) {
        Some(why) => Err(why),
        None => Ok(kind.unwrap_or(Kind::Png)),
    }
}

/// The summary at the top of a report and the issue text.
pub fn summary(input: &Input) -> String {
    format!("Recall {}\nWindows {}\n", input.version, input.windows)
}

/// Build the report as `dir\report-<n>.zip` (the next free number).
pub fn build(input: &Input, dir: &Path, names: &Names) -> Result<Built, String> {
    let mut entries = vec![zip::Entry { name: "report.txt".into(), data: format!("{}\n{}\n", summary(input), input.text.trim()).into_bytes() }];
    if let Some(trace) = &input.trace_dir {
        let mut found: Vec<PathBuf> = fs::read_dir(trace).into_iter().flatten().flatten().map(|e| e.path()).collect();
        found.sort();
        for path in found {
            let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
            let keep = name.ends_with(".trace") || name.ends_with(".trace.1") || (name.starts_with("crash-") && name.ends_with(".json"));
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
        let data = media::clean(kind, data).ok_or("An added picture is damaged.")?;
        entries.push(zip::Entry { name: format!("media/media-{}.{}", i + 1, kind.ext()), data });
    }
    let total: u64 = entries.iter().map(|e| e.data.len() as u64).sum();
    if total > MAX_REPORT {
        return Err("The report is over 24 MB, the most GitHub takes. Remove a clip or add a shorter one.".into());
    }
    fs::create_dir_all(dir).map_err(|e| format!("Recall could not make the reports folder ({e})."))?;
    let next = (1..).find(|n| !dir.join(format!("report-{n}.zip")).exists()).unwrap_or(1);
    let path = dir.join(format!("report-{next}.zip"));
    let mut file = fs::File::create(&path).map_err(|e| format!("Recall could not save the report ({e})."))?;
    let bytes = zip::write(&mut file, &entries).map_err(|e| format!("Recall could not save the report ({e})."))?;
    let files = entries.iter().map(|e| (e.name.clone(), e.data.len() as u64)).collect();
    Ok(Built { path, files, bytes })
}

/// The new-issue page on GitHub with the title and text filled in. The person attaches the
/// zip there themselves; the address carries only what they typed and the summary.
pub fn issue_url(input: &Input) -> String {
    let mut text: String = input.text.trim().chars().take(3000).collect();
    if text.is_empty() {
        text = "(no description)".into();
    }
    let body = format!("{text}\n\n---\n{}\nThe report zip is attached below.\n", summary(input));
    let title: String = input.text.trim().lines().next().unwrap_or("Bug report").chars().take(80).collect();
    format!("https://github.com/LoneBeast27/recall/issues/new?title={}&body={}", encode(&title), encode(&body))
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

/// Remove this PC's trace and crash records (the person's own data, on request). Clips and
/// settings are never touched; only `*.trace`, `*.trace.1`, `crash-*` files in `dir`.
pub fn clear_trace(dir: &Path) -> usize {
    let mut removed = 0;
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = name.ends_with(".trace") || name.ends_with(".trace.1") || (name.starts_with("crash-") && (name.ends_with(".json") || name.ends_with(".pending")));
        if ours && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests;
