//! Settings "Reset cache size" over a damaged storage.json (RI3-1, F-storage-5). The size it
//! writes is shown on the button first, and it is the user's own number whenever one can be
//! found: the budget read earlier this session, else the `cache_budget_bytes` number still
//! legible in the damaged bytes or in a `.bad` copy, else (said so on screen) the first-run
//! pick. A storage.json damaged before the first read no longer turns a 20 GiB budget into 2 GiB.
use std::path::{Path, PathBuf};

use super::evict as lru;

/// Where the offered size comes from (shown beside the button).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetFrom {
    /// The budget storage.json held when it was last read in this session.
    LastRead,
    /// Recovered from the damaged file or one of its `.bad` copies.
    Recovered(PathBuf),
    /// No size could be recovered: the first-run pick for this drive.
    Default,
}

/// The size "Reset cache size" would write, and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetOffer {
    pub bytes: u64,
    pub from: ResetFrom,
    /// The recovered file marked its budget as the first-run pick (`Settings::first_run`).
    first_run: bool,
}

impl ResetOffer {
    /// One line for under the button.
    pub fn source_line(&self) -> String {
        match &self.from {
            ResetFrom::LastRead => "The size read earlier in this session.".into(),
            ResetFrom::Recovered(file) => format!("Recovered from {}.", file.file_name().map_or_else(|| file.display().to_string(), |n| n.to_string_lossy().into_owned())),
            ResetFrom::Default => "No size could be recovered from the damaged file: this is the default for this drive.".into(),
        }
    }
}

/// storage.json is damaged and a `.bad` copy of it exists (never a merely locked file).
pub(super) fn budget_resettable(root: &Path) -> bool {
    matches!(lru::load_settings(root), lru::Stored::Damaged(_))
}

/// What Reset would write now; None when storage.json is not damaged (or has no backup yet).
pub(super) fn reset_offer(root: &Path) -> Option<ResetOffer> {
    if !budget_resettable(root) {
        return None;
    }
    if let Some(bytes) = super::video::remembered(root) {
        return Some(ResetOffer { bytes, from: ResetFrom::LastRead, first_run: false });
    }
    for file in damaged_then_bads(root) {
        if let Some((bytes, first_run)) = std::fs::read(&file).ok().and_then(|data| salvage(&data)) {
            return Some(ResetOffer { bytes, from: ResetFrom::Recovered(file), first_run });
        }
    }
    Some(ResetOffer { bytes: super::video::first_run_budget(root), from: ResetFrom::Default, first_run: true })
}

/// Writes `shown` (the size on the button) durably over the damaged storage.json. Refused when
/// the file is not damaged, or when the size Reset would write is no longer the one shown.
pub(super) fn reset_budget(root: &Path, shown: u64) -> Result<u64, String> {
    let Some(offer) = reset_offer(root) else {
        return Err("storage.json is not damaged (or has no backup yet); cache size not reset".into());
    };
    if offer.bytes != shown {
        return Err("The size to restore changed since it was shown; check it and try again.".into());
    }
    let settings = lru::Settings { cache_budget_bytes: Some(offer.bytes), first_run: offer.first_run.then_some(true) };
    lru::write_settings(root, &settings)?;
    Ok(offer.bytes)
}

/// storage.json itself, then its `.bad*` copies newest first.
fn damaged_then_bads(root: &Path) -> Vec<PathBuf> {
    let mut bads: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(root)
        .map(|listing| {
            listing
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("storage.json.bad"))
                .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
                .collect()
        })
        .unwrap_or_default();
    bads.sort_by(|a, b| b.0.cmp(&a.0));
    std::iter::once(root.join("storage.json")).chain(bads.into_iter().map(|(_, p)| p)).collect()
}

/// Lenient read of `"cache_budget_bytes": <digits>` anywhere in damaged bytes (a cut-off write,
/// NUL runs, a broken brace). A number that does not fit u64, or 0, is not a budget. Returns it
/// with whether `"first_run": true` also survives.
pub(super) fn salvage(data: &[u8]) -> Option<(u64, bool)> {
    let bytes = number_after(data, b"\"cache_budget_bytes\"")?;
    let first_run = value_after(data, b"\"first_run\"").is_some_and(|rest| rest.starts_with(b"true"));
    Some((bytes, first_run))
}

/// The bytes after `key`, blanks, `:` and blanks, at the first place where that shape holds.
fn value_after<'a>(data: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let blank = |b: &u8| b.is_ascii_whitespace();
    (0..data.len()).filter(|&at| data[at..].starts_with(key)).find_map(|at| {
        let rest = &data[at + key.len()..];
        let rest = &rest[rest.iter().take_while(|b| blank(b)).count()..];
        let rest = rest.strip_prefix(b":")?;
        Some(&rest[rest.iter().take_while(|b| blank(b)).count()..])
    })
}

fn number_after(data: &[u8], key: &[u8]) -> Option<u64> {
    let rest = value_after(data, key)?;
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    // A number cut off by the damage ("21474" then NULs) is not the user's number.
    let whole = rest.get(digits).is_some_and(|end| matches!(end, b',' | b'}' | b' ' | b'\t' | b'\r' | b'\n'));
    let value: u64 = std::str::from_utf8(&rest[..digits]).ok()?.parse().ok()?;
    (whole && value > 0).then_some(value)
}
